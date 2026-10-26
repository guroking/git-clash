use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame, Terminal,
};
use serde_json::{json, Value};
use std::{
    env, fs,
    io::{self, Read},
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

#[derive(Clone, Debug)]
enum Resolution {
    Unresolved,
    Ours,
    Theirs,
    BothOursFirst,
    BothTheirsFirst,
    Custom(Vec<String>),
    AiMerged {
        lines: Vec<String>,
        diagnosis: String,
        risk_badge: String,
    },
}

#[derive(Clone, Debug)]
struct ConflictHunk {
    ours_label: String,
    theirs_label: String,
    ours_lines: Vec<String>,
    theirs_lines: Vec<String>,
    resolution: Resolution,
    history: Vec<Resolution>,
}

impl ConflictHunk {
    fn resolved_lines(&self) -> Vec<String> {
        match &self.resolution {
            Resolution::Unresolved => {
                let mut out = vec![format!("<<<<<<< {}", self.ours_label)];
                out.extend(self.ours_lines.clone());
                out.push("=======".to_string());
                out.extend(self.theirs_lines.clone());
                out.push(format!(">>>>>>> {}", self.theirs_label));
                out
            }
            Resolution::Ours => self.ours_lines.clone(),
            Resolution::Theirs => self.theirs_lines.clone(),
            Resolution::BothOursFirst => {
                let mut out = self.ours_lines.clone();
                out.extend(self.theirs_lines.clone());
                out
            }
            Resolution::BothTheirsFirst => {
                let mut out = self.theirs_lines.clone();
                out.extend(self.ours_lines.clone());
                out
            }
            Resolution::Custom(lines) => lines.clone(),
            Resolution::AiMerged { lines, .. } => lines.clone(),
        }
    }

    fn is_resolved(&self) -> bool {
        !matches!(self.resolution, Resolution::Unresolved)
    }

    fn set_resolution(&mut self, next: Resolution) {
        let prev = self.resolution.clone();
        self.history.push(prev);
        self.resolution = next;
    }

    fn undo(&mut self) -> bool {
        if let Some(prev) = self.history.pop() {
            self.resolution = prev;
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug)]
enum Segment {
    Normal(Vec<String>),
    Conflict(ConflictHunk),
}

#[derive(Clone, Debug)]
struct ConflictFile {
    path: PathBuf,
    segments: Vec<Segment>,
    conflict_indices: Vec<usize>,
    current_hunk: usize,
    saved_and_staged: bool,
}

impl ConflictFile {
    fn parse(path: PathBuf) -> Result<Self> {
        let raw = fs::read_to_string(&path)?;
        #[derive(PartialEq)]
        enum ParseState { Normal, Ours, Base, Theirs }
        let mut state = ParseState::Normal;
        let mut segments = Vec::new();
        let mut normal_buf = Vec::new();
        let mut ours_buf = Vec::new();
        let mut theirs_buf = Vec::new();
        let mut ours_label = String::new();

        for line in raw.lines() {
            if state == ParseState::Normal && line.starts_with("<<<<<<<") {
                if !normal_buf.is_empty() {
                    segments.push(Segment::Normal(std::mem::take(&mut normal_buf)));
                }
                ours_label = line.trim_start_matches('<').trim().to_string();
                state = ParseState::Ours;
            } else if state == ParseState::Ours && line.starts_with("|||||||") {
                state = ParseState::Base;
            } else if (state == ParseState::Ours || state == ParseState::Base) && line.starts_with("=======") {
                state = ParseState::Theirs;
            } else if state == ParseState::Theirs && line.starts_with(">>>>>>>") {
                let theirs_label = line.trim_start_matches('>').trim().to_string();
                segments.push(Segment::Conflict(ConflictHunk {
                    ours_label: std::mem::take(&mut ours_label),
                    theirs_label,
                    ours_lines: std::mem::take(&mut ours_buf),
                    theirs_lines: std::mem::take(&mut theirs_buf),
                    resolution: Resolution::Unresolved,
                    history: Vec::new(),
                }));
                state = ParseState::Normal;
            } else {
                match state {
                    ParseState::Normal => normal_buf.push(line.to_string()),
                    ParseState::Ours => ours_buf.push(line.to_string()),
                    ParseState::Base => {}
                    ParseState::Theirs => theirs_buf.push(line.to_string()),
                }
            }
        }
        if !normal_buf.is_empty() {
            segments.push(Segment::Normal(normal_buf));
        }
        let conflict_indices: Vec<usize> = segments.iter().enumerate().filter_map(|(i, s)| matches!(s, Segment::Conflict(_)).then_some(i)).collect();
        Ok(Self { path, segments, conflict_indices, current_hunk: 0, saved_and_staged: false })
    }

    fn current_hunk_ref(&self) -> Option<&ConflictHunk> {
        let seg_idx = *self.conflict_indices.get(self.current_hunk)?;
        match &self.segments[seg_idx] { Segment::Conflict(h) => Some(h), _ => None }
    }

    fn current_hunk_mut(&mut self) -> Option<&mut ConflictHunk> {
        let seg_idx = *self.conflict_indices.get(self.current_hunk)?;
        match &mut self.segments[seg_idx] { Segment::Conflict(h) => Some(h), _ => None }
    }

    fn resolved_count(&self) -> usize {
        self.conflict_indices.iter().filter(|&&idx| match &self.segments[idx] { Segment::Conflict(h) => h.is_resolved(), _ => false }).count()
    }

    fn total_conflicts(&self) -> usize { self.conflict_indices.len() }
    fn all_resolved(&self) -> bool { self.resolved_count() == self.total_conflicts() }

    fn render_full_file(&self) -> String {
        let mut out = Vec::new();
        for seg in &self.segments {
            match seg {
                Segment::Normal(lines) => out.extend(lines.clone()),
                Segment::Conflict(h) => out.extend(h.resolved_lines()),
            }
        }
        out.join("\n") + "\n"
    }
}

struct AiResponseMsg {
    file_idx: usize,
    hunk_idx: usize,
    result: Result<(Vec<String>, String, String), String>,
}

struct App {
    files: Vec<ConflictFile>,
    current_file: usize,
    edit_mode: bool,
    edit_buffer: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    scroll_y: u16,
    status_msg: String,
    opsie_banner: String,
    ai_loading: bool,
    ai_tx: Sender<AiResponseMsg>,
    ai_rx: Receiver<AiResponseMsg>,
}

impl App {
    fn new(files: Vec<ConflictFile>) -> Self {
        let (ai_tx, ai_rx) = mpsc::channel();
        Self {
            files,
            current_file: 0,
            edit_mode: false,
            edit_buffer: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
            scroll_y: 0,
            status_msg: "단축키: [o]Ours [t]Theirs [b]Both [a]AI병합 [e]수정 [u]실행취소 [n/p]이동 [Enter]저장".to_string(),
            opsie_banner: "💡 [opsie] 'a' 키를 누르면 AI가 충돌 원인과 위험도를 1초 만에 분석합니다.".to_string(),
            ai_loading: false,
            ai_tx,
            ai_rx,
        }
    }

    fn current_file_ref(&self) -> &ConflictFile { &self.files[self.current_file] }
    fn current_file_mut(&mut self) -> &mut ConflictFile { &mut self.files[self.current_file] }

    fn start_inline_edit(&mut self) {
        if let Some(hunk) = self.current_file_ref().current_hunk_ref() {
            let mut lines = hunk.resolved_lines();
            if matches!(hunk.resolution, Resolution::Unresolved) { lines = hunk.ours_lines.clone(); }
            if lines.is_empty() { lines.push(String::new()); }
            self.edit_buffer = lines;
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.edit_mode = true;
            self.status_msg = "✏️ [인라인 편집] Ctrl+S 적용 | Esc 취소".to_string();
        }
    }

    fn trigger_ai_smart_merge(&mut self) {
        if self.ai_loading { return; }
        let file_idx = self.current_file;
        let file_path = self.current_file_ref().path.display().to_string();
        let hunk_idx = self.current_file_ref().current_hunk;
        let Some(hunk) = self.current_file_ref().current_hunk_ref() else { return; };
        let ours_code = hunk.ours_lines.join("\n");
        let theirs_code = hunk.theirs_lines.join("\n");
        let ours_label = hunk.ours_label.clone();
        let theirs_label = hunk.theirs_label.clone();
        let tx = self.ai_tx.clone();
        self.ai_loading = true;
        self.opsie_banner = "🤖 [opsie AI 가동 중...]".to_string();

        thread::spawn(move || {
            let res = request_hybrid_ai_merge(&file_path, &ours_label, &theirs_label, &ours_code, &theirs_code);
            let _ = tx.send(AiResponseMsg { file_idx, hunk_idx, result: res });
        });
    }

    fn poll_ai_response(&mut self) {
        if let Ok(msg) = self.ai_rx.try_recv() {
            self.ai_loading = false;
            match msg.result {
                Ok((lines, diag, risk)) => {
                    if let Some(file) = self.files.get_mut(msg.file_idx) {
                        if let Some(&seg_idx) = file.conflict_indices.get(msg.hunk_idx) {
                            if let Segment::Conflict(hunk) = &mut file.segments[seg_idx] {
                                hunk.set_resolution(Resolution::AiMerged { lines, diagnosis: diag.clone(), risk_badge: risk.clone() });
                            }
                        }
                    }
                    self.opsie_banner = format!("🤖 [{}] {}", risk, diag);
                    self.status_msg = "✅ AI 스마트 병합 완료! [Enter] 저장".to_string();
                }
                Err(e) => { self.opsie_banner = format!("⚠️ AI 실패: {}", e); }
            }
        }
    }
}

fn request_hybrid_ai_merge(file_path: &str, ours_label: &str, theirs_label: &str, ours_code: &str, theirs_code: &str) -> Result<(Vec<String>, String, String), String> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| "OPENAI_API_KEY 없음".to_string())?;
    let prompt = format!("File: {file_path}\nOurs({ours_label}):\n{ours_code}\nTheirs({theirs_label}):\n{theirs_code}\nRespond in JSON format: {{\"risk_badge\": \"SAFE\", \"diagnosis\": \"한줄요약\", \"merged_code\": \"병합된코드\"}}");
    let payload = json!({
        "model": "gpt-4o-mini",
        "response_format": { "type": "json_object" },
        "messages": [{ "role": "user", "content": prompt }]
    });
    let resp = ureq::post("https://api.openai.com/v1/chat/completions")
        .set("Authorization", &format!("Bearer {}", api_key.trim()))
        .send_json(payload)
        .map_err(|e| e.to_string())?;
    let body: Value = resp.into_json().map_err(|e| e.to_string())?;
    let content = body["choices"][0]["message"]["content"].as_str().unwrap_or("{}");
    let parsed: Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let risk = parsed["risk_badge"].as_str().unwrap_or("SAFE").to_string();
    let diag = parsed["diagnosis"].as_str().unwrap_or("병합됨").to_string();
    let lines = parsed["merged_code"].as_str().unwrap_or("").lines().map(|s| s.to_string()).collect();
    Ok((lines, diag, risk))
}

fn run_opsie_cli_diagnose(query: &str) -> Result<()> {
    let api_key = env::var("OPENAI_API_KEY")?;
    let prompt = format!("Query: {query}\nJSON: {{\"risk\": \"SAFE\", \"cause\": \"원인\", \"command\": \"명령어\"}}");
    let payload = json!({ "model": "gpt-4o-mini", "response_format": { "type": "json_object" }, "messages": [{ "role": "user", "content": prompt }] });
    let resp = ureq::post("https://api.openai.com/v1/chat/completions").set("Authorization", &format!("Bearer {}", api_key.trim())).send_json(payload)?;
    let body: Value = resp.into_json()?;
    let parsed: Value = serde_json::from_str(body["choices"][0]["message"]["content"].as_str().unwrap_or("{}"))?;
    println!("위험도: [{}] | 원인: {} | 해결: $ {}", parsed["risk"], parsed["cause"], parsed["command"]);
    Ok(())
}

fn find_conflicted_files() -> Result<Vec<PathBuf>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() { return Ok(args.into_iter().map(PathBuf::from).collect()); }
    let output = Command::new("git").args(["diff", "--name-only", "--diff-filter=U"]).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.trim().is_empty()).map(|l| PathBuf::from(l.trim())).collect())
}

fn draw_ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(4), Constraint::Percentage(46), Constraint::Percentage(42), Constraint::Length(3)]).split(f.area());
    let tabs: Vec<Span> = app.files.iter().enumerate().map(|(i, f)| Span::styled(format!(" [{}] ", f.path.display()), if i == app.current_file { Style::default().fg(Color::Black).bg(Color::Cyan) } else { Style::default().fg(Color::Gray) })).collect();
    f.render_widget(Paragraph::new(vec![Line::from(tabs), Line::from(Span::styled(&app.opsie_banner, Style::default().fg(Color::Yellow)))]).block(Block::default().borders(Borders::ALL).title(" clash + opsie ")), chunks[0]);

    let mid = Layout::default().direction(Direction::Horizontal).constraints([Constraint::Percentage(50), Constraint::Percentage(50)]).split(chunks[1]);
    if let Some(hunk) = app.current_file_ref().current_hunk_ref() {
        let ours: Vec<Line> = hunk.ours_lines.iter().enumerate().map(|(i, l)| Line::from(format!("{:3} │ {}", i+1, l))).collect();
        let theirs: Vec<Line> = hunk.theirs_lines.iter().enumerate().map(|(i, l)| Line::from(format!("{:3} │ {}", i+1, l))).collect();
        f.render_widget(Paragraph::new(ours).block(Block::default().borders(Borders::ALL).title(" OURS ")).wrap(Wrap{trim:false}), mid[0]);
        f.render_widget(Paragraph::new(theirs).block(Block::default().borders(Borders::ALL).title(" THEIRS ")).wrap(Wrap{trim:false}), mid[1]);

        let res_lines: Vec<Line> = if app.edit_mode {
            app.edit_buffer.iter().enumerate().map(|(r, l)| Line::from(format!("{:3} │ {}", r+1, l))).collect()
        } else {
            hunk.resolved_lines().iter().enumerate().map(|(i, l)| Line::from(format!("{:3} │ {}", i+1, l))).collect()
        };
        f.render_widget(Paragraph::new(res_lines).block(Block::default().borders(Borders::ALL).title(" RESULT ")).wrap(Wrap{trim:false}), chunks[2]);
    }
    f.render_widget(Paragraph::new(Line::from(Span::styled(&app.status_msg, Style::default().fg(Color::Cyan)))).block(Block::default().borders(Borders::ALL)), chunks[3]);
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() >= 2 && (args[1] == "--diagnose" || args[1] == "-d") {
        return run_opsie_cli_diagnose(&args[2..].join(" "));
    }
    let paths = find_conflicted_files()?;
    if paths.is_empty() { println!("충돌 파일 없음"); return Ok(()); }
    let mut files = Vec::new();
    for p in paths { files.push(ConflictFile::parse(p)?); }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(files);

    loop {
        app.poll_ai_response();
        terminal.draw(|f| draw_ui(f, &app))?;
        if !event::poll(Duration::from_millis(50))? { continue; }
        let Event::Key(key) = event::read()? else { continue; };
        if key.kind != KeyEventKind::Press { continue; }

        if app.edit_mode {
            match key.code {
                KeyCode::Esc => { app.edit_mode = false; }
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let buf = app.edit_buffer.clone();
                    if let Some(h) = app.current_file_mut().current_hunk_mut() { h.set_resolution(Resolution::Custom(buf)); }
                    app.edit_mode = false;
                }
                KeyCode::Enter => { app.edit_buffer.push(String::new()); }
                KeyCode::Backspace => { app.edit_buffer.pop(); }
                KeyCode::Char(c) => { if let Some(l) = app.edit_buffer.last_mut() { l.push(c); } }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Tab => { app.current_file = (app.current_file + 1) % app.files.len(); }
            KeyCode::Char('o') => { if let Some(h) = app.current_file_mut().current_hunk_mut() { h.set_resolution(Resolution::Ours); } }
            KeyCode::Char('t') => { if let Some(h) = app.current_file_mut().current_hunk_mut() { h.set_resolution(Resolution::Theirs); } }
            KeyCode::Char('b') => { if let Some(h) = app.current_file_mut().current_hunk_mut() { h.set_resolution(Resolution::BothOursFirst); } }
            KeyCode::Char('u') => { if let Some(h) = app.current_file_mut().current_hunk_mut() { h.undo(); } }
            KeyCode::Char('e') => { app.start_inline_edit(); }
            KeyCode::Char('a') => { app.trigger_ai_smart_merge(); }
            KeyCode::Enter => {
                let content = app.current_file_ref().render_full_file();
                let path = app.current_file_ref().path.clone();
                fs::write(&path, content)?;
                Command::new("git").args(["add", &path.to_string_lossy()]).output()?;
                break;
            }
            _ => {}
        }
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}