# Repository Guidelines

## Project Structure

`src/` contains the Rust CLI and shared bridge core; `src/bridge/` holds protocol, runtime, IPC, and storage modules. `gui/src/` contains the React/TypeScript interface, and `gui/src-tauri/` is its Rust desktop wrapper. Rust integration tests live in `tests/`; frontend tests sit beside their source as `*.test.ts`. Python scripts in `tests/` exercise the real CLI and browser workflows. `docs/protocol.md` defines the current API contract; older plans and UI mockups are reference material.

## Build, Test, and Development

- `cargo fmt --all -- --check` checks Rust formatting.
- `cargo clippy --workspace --all-targets -- -D warnings` checks Rust code and tests.
- `cargo test --workspace --no-run` compiles tests; `timeout 60s cargo test --workspace` runs them.
- `cargo build --bin sessionbridge` builds the CLI used by protocol scripts.
- `cd gui && npm ci && npm run dev` installs frontend dependencies and starts Vite; `npm run tauri dev` starts the desktop app.
- From `gui/`, run `npm test` and `npm run build` for frontend tests and the TypeScript/Vite build. The full UI integration check is `python3 tests/bridge_ui.py target/debug/sessionbridge` while Vite is running.

## Style and Tests

Use four spaces in Rust and TypeScript, matching `.editorconfig`; JSON and TOML use two. Run `cargo fmt` before submitting Rust changes. Keep Rust modules and functions `snake_case`, React components `PascalCase`, and tests descriptive (Rust `#[test]` names or Vitest `*.test.ts`). Add tests at the layer that owns the behavior. Backend tests should finish within 60 seconds after compilation. UI integration tests must use the real Rust backend; do not describe simulated checks as real provider integrations.

## Changes and Pull Requests

The available Git history does not establish a commit-message convention. Use a short imperative subject, for example `Fix workgroup initialization`. PRs should explain the behavior change, list the checks run, link relevant issues, and include screenshots for visible UI changes. Update `docs/protocol.md` when changing the API contract.

## Security and Configuration

Keep SQLite authoritative and use parameterized SQL, structured process arguments, private files, and explicit failures. Do not silently switch providers or bypass the required sandbox. See `CONTRIBUTING.md` and `SECURITY.md` before changing these boundaries.
