# Contributing

SessionBridge v2 has one Rust application core shared by its local service, CLI,
MCP adapter and Tauri desktop application.

- Keep authentication, routing, work state and acceptance rules in the core.
- SQLite is authoritative. Store bodies once; change records contain references.
- Use structured argv, parameterized SQL, private files, explicit failures and
  bounded process lifetimes. Never silently fall back to another model/provider.
- Retain user changes; use isolated worktrees and verify the current target HEAD.
- Tests must assert the current workgroup contract, not the retired invitation
  and all-workers-concluded workflow.
- Backend tests default to a 60-second timeout after compilation.
- Browser tests must use the real backend and actually exercise the page.

See README.md for build and validation commands and docs/protocol.md for the
current API. Native CLI tests and paid provider tests are separate; a simulated
protocol test must never be reported as a successful real-model integration.
