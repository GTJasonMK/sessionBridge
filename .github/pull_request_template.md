## Summary

Describe the user-visible change and why it is needed.

## Invariants And Compatibility

- Affected state-machine, schema, CLI/API, Git, security, or completion invariants:
- Migration or compatibility impact:
- New fallback, alias, or duplicated rule introduced: none / explain

## Verification

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `timeout 60s cargo test --workspace`
- [ ] `cd gui && npm test`
- [ ] `cd gui && npm run build`
- [ ] Tauri build or manual GUI verification when applicable
- [ ] Documentation and machine contracts updated when applicable
- [ ] Logs, screenshots, and fixtures contain no real secrets or private IDs
