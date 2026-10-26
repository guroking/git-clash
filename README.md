# clash (`git-clash`)

> A keyboard-first terminal UI for reviewing and resolving Git merge conflicts.

`clash` is a Rust TUI for inspecting conflicted files, comparing the current and incoming changes, choosing a resolution, and staging the result from the terminal.

**Project status: experimental.** The repository currently contains an initial implementation. The feature descriptions below distinguish code observed in the current source from capabilities that are not yet implemented or verified.

## What it does

The current implementation is a single-binary Rust application using Ratatui and Crossterm. It can:

- Discover unmerged paths using `git diff --name-only --diff-filter=U`, or accept file paths as command-line arguments.
- Parse standard textual conflict markers into normal and conflict sections, including the optional diff3 base section.
- Display the current (`Ours`) and incoming (`Theirs`) contents with a result preview.
- Choose `Ours` (`o`), `Theirs` (`t`), or both with Ours first (`b`).
- Undo the most recent resolution for the active hunk (`u`).
- Request an optional OpenAI suggestion with `a`, provided `OPENAI_API_KEY` is set.
- Save the current file and invoke `git add` with `Enter`.
- Run a separate OpenAI diagnostic request with `--diagnose` or `-d`.

The implementation is experimental and has important limitations. In particular, the current `Enter` path does not block unresolved hunks, does not check the `git add` exit status, and writes the file directly. Review the complete diff and keep a backup or clean Git checkpoint before using it on important work.

## Quick start

Requirements: Rust/Cargo and Git.

```bash
cargo run
```

With no arguments, the program looks for unmerged paths in the current repository. To pass paths explicitly:

```bash
cargo run -- src/example.rs
```

The explicit-path mode expects files containing textual Git conflict markers. It does not currently read Git index stages to reconstruct `Base`, `Ours`, and `Theirs` independently.

## Keyboard controls

| Key | Current behavior |
| --- | --- |
| `o` | Choose Ours for the active hunk |
| `t` | Choose Theirs for the active hunk |
| `b` | Keep both, Ours first |
| `u` | Undo the active hunk's latest resolution |
| `a` | Request an OpenAI merge suggestion |
| `e` | Enter the current inline-edit mode |
| `Tab` | Select the next file |
| `Enter` | Write the current file and attempt `git add` |
| `q` | Quit the TUI |

The current source displays `n`/`p` navigation hints, but those keys are not handled by the event loop. The inline editor is rudimentary and is not a full text editor. `Space`, Both-order selection, previous-file navigation, and a Base-view toggle are not currently implemented.

## AI features and privacy

The AI request sends the active file path and conflict-side contents to `https://api.openai.com/v1/chat/completions` using the `gpt-4o-mini` model. Do not use this feature with confidential code unless your policy permits sending that content to the provider. The current implementation does not provide a local Ollama fallback, a pre-send redaction guarantee, or robust validation that a returned merge is correct. AI output is a suggestion, not a correctness or safety guarantee.

Set the key in the environment rather than committing it:

```bash
export OPENAI_API_KEY="..."
```

The diagnostic mode also makes a remote OpenAI request:

```bash
cargo run -- --diagnose "Describe the error"
```

## Build and checks

```bash
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
```

These are suggested checks; no CI or test results are claimed by this document.

## Known limitations and safety notes

- The conflict parser is a small textual parser, not a complete Git merge engine. Malformed markers and unusual file encodings need more robust handling.
- The current UI does not implement the advertised `n`/`p` hunk navigation.
- The inline editor lacks normal cursor movement and editing behavior; use a regular editor for careful changes.
- `Enter` writes directly to the file and then invokes `git add`; unresolved-conflict checks, atomic replacement, and verification of the Git command result are not implemented.
- The `git add` subprocess exit code is not checked.
- The AI path sends code to a remote service and does not guarantee semantic correctness.
- There is no verified Ollama fallback, syntax highlighting, or Git mergetool argument support in the inspected source.
- Build, test, and cross-platform results have not been verified here.

Use Git checkpoints and inspect the staged result before committing:

```bash
git diff --check
git diff --cached
git status
```

## Roadmap

1. Add parser and resolver tests for multiple hunks, diff3 markers, malformed input, and trailing-newline behavior.
2. Prevent saving when any hunk is unresolved; use safe file replacement and report staging failures.
3. Implement real hunk navigation and replace the rudimentary editor with `$EDITOR` integration or a maintained text widget.
4. Read Git index stages for reliable Base/Ours/Theirs context and support the standard `git mergetool` protocol.
5. Add syntax highlighting and robust, explicitly opt-in AI provider handling.
6. Add CI and document only verified platforms and behavior.

## Contributing

Focused fixes, parser tests, error handling, documentation, and reproducible bug reports are welcome. Include the operating system, Rust and Git versions, exact command, expected result, and actual result in bug reports. Never include API keys or confidential source code.

## License

No license information was confirmed during this repository review. Check the repository's license before redistributing or relying on permissions to reuse this project.
