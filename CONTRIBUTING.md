# Contributing to SessionBridge

SessionBridge coordinates autonomous coding agents through a local SQLite
state machine, Git worktrees, and a stable machine-readable CLI. Contributions
must preserve those consistency and security boundaries.

## Development Setup

Required tools:

- Rust 1.95.0 with `rustfmt` and `clippy` (installed from
  `rust-toolchain.toml`).
- Node.js 20.19 or newer and npm 10 or newer.
- Git.
- Linux desktop dependencies for Tauri development:
  `libwebkit2gtk-4.1-dev`, `libappindicator3-dev`, `librsvg2-dev`, `patchelf`,
  and `xdg-utils`.

Install frontend dependencies once:

```bash
cd gui
npm ci
```

## Quality Checks

Run the same checks as CI before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-run
timeout 60s cargo test --workspace

cd gui
npm test
npm run build
npm run tauri build -- --no-bundle
```

Do not treat a passing existing test as sufficient evidence. Review whether
the test still expresses the current protocol and add a focused regression
test for changed behavior.

## Architecture Rules

- Keep each business invariant in one application-service or domain location.
  CLI handlers, GUI commands, and MCP tools must call the same use case.
- Do not add silent fallback paths, compatibility aliases, duplicated SQL
  projections, or handler-level post-write compensation.
- State changes that affect Git integrity, Requirements, Deliverables, or Task
  completion must pass through the shared runtime reconciliation boundary.
- SQLite migrations are append-only. Never edit an already released migration;
  add the next `schema_vN.sql` migration and update migration tests.
- Events are an audit stream, not a second source of current business state.
- Machine output contracts, error codes, request idempotency, and stdout/stderr
  separation are public behavior. Update protocol and CLI contract tests when
  they change.
- Never log or commit invitation tokens, Session tokens, context files,
  database files, private prompts, or local recovery artifacts.
- Keep changes scoped. Unrelated formatting, generated output, and dependency
  churn should not be included in the same pull request.

## Pull Requests

Describe the user-visible behavior, affected invariants, migration or API
impact, and commands used for verification. Include screenshots only for GUI
changes where visual behavior matters, and remove all real Task, repository,
Session, and invitation identifiers.

By contributing, you agree that your contribution is licensed under the
project's MIT OR Apache-2.0 license.

Security vulnerabilities must follow [SECURITY.md](SECURITY.md) and must not be
reported in a public issue.
