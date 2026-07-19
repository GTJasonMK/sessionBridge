# SessionBridge

[![CI](https://github.com/GTJasonMK/sessionBridge/actions/workflows/ci.yml/badge.svg)](https://github.com/GTJasonMK/sessionBridge/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)

SessionBridge is a local-first coordination CLI for autonomous coding agents. A
user publishes one task and creates one invitation per Agent session. The
Agents then use the shared task state to discover gaps, propose work, claim it,
coordinate resources, submit evidence, and decide when their own participation
is finished.

The coordination database is local SQLite in WAL mode. A local daemon delivers
durable wait callbacks and MCP reconnect results; no hosted service is required.

> **Project status:** SessionBridge is pre-1.0 software. State migrations are
> tested and append-only, but machine protocol and CLI changes may still be
> breaking until a stable release is published.

## Core Workflow

The normal user workflow starts from a Git repository with a committed
baseline. Worker invitations create isolated worktrees, so an empty repository
or an uncommitted implementation plan cannot be used as the Agent baseline:

```bash
# 1. Create the Git repository.
git init

# 2. Initialize SessionBridge, then commit the complete project baseline.
sessionbridge init --name SessionBridge
git add .
git commit -m "Initialize project baseline"

# 3. Publish the Task and create four worker invitations in one recoverable operation.
sessionbridge launch --brief docs/IMPLEMENTATION_PLAN.md
```

`launch` uses the complete brief as the immutable Objective, derives the title
from its first H1 or file name, defaults to `bounded`, creates four worker
invitations valid for two hours, and prints one stable `prompt show` command per
Agent. Send a different prompt to every Agent, then observe the Task with the
printed command:

```bash
sessionbridge prompt show <TASK_ID> --batch <BATCH_ID> --index 1
sessionbridge watch <TASK_ID>
```

For a short task, pass the Objective directly:

```bash
sessionbridge launch "Implement registration, login, logout, and tests"
```

Run `sessionbridge launch` with no brief in an interactive terminal to answer
the fields one by one and confirm a summary before publication. Machine output
and `--non-interactive` require an explicit brief. `launch --dry-run` performs
all Git and workspace checks without writing Task or Invitation state.

The repository must be clean by default because Agent worktrees start from the
committed baseline. `--allow-dirty-baseline` is an explicit exception and
returns the paths that Agents will not see.

If Task publication succeeds but Invitation or Prompt creation fails, the
error contains a private launch ID and resume argv:

```bash
sessionbridge launch resume <LAUNCH_ID>
```

The same launch request never creates duplicate Tasks or Invitation batches.

Strict or scripted contracts remain available through the lower-level API:

```bash
sessionbridge task publish \
  --title "Implement the autonomous coordination MVP" \
  --objective-file objective.md \
  --constraints-file constraints.md \
  --done-file definition-of-done.md \
  --autonomy bounded \
  --format json
```

The prompt does not contain the invitation token. It points to a private,
one-time invitation file and tells the Agent to run `agent join`, switch to the
returned worktree, then use the returned context path for `agent whoami` and
`agent next`.

Each Agent chooses a short nickname when joining. The same canonical Session
label is shown in chat, participant lists, diagnostics, and audit projections:

```bash
sessionbridge agent join --invite-file <INVITE> --nickname "Build Reviewer" \
  --provision-worktree --format json --non-interactive
```

For an explicitly approved single-recipient channel, `--secret-delivery inline
--allow-secret-output --format text` emits a one-off prompt containing the
secret on stdout. Inline delivery is rejected for batches and machine output.

When exactly one Task is active or paused, user-side Task references accept
`current`:

```bash
sessionbridge status
sessionbridge task show current
sessionbridge prompt list current
sessionbridge watch
```

Multiple open Tasks make `current` fail with `CURRENT_TASK_AMBIGUOUS`; the CLI
never silently selects the newest Task.

Project launch defaults and named profiles live in the optional, committable
`.sessionbridge/settings.toml`:

```bash
sessionbridge config init
# Edit settings.toml to add project-specific profiles, then commit it.
sessionbridge config validate
git add .sessionbridge/settings.toml
git commit -m "Configure SessionBridge launch profiles"
sessionbridge launch --brief task.md --profile large
```

## Agent Loop

An Agent does not receive a permanent role or a fixed subtask from the user.
After joining, it repeatedly reads the global state and selects work that is
currently useful and safe:

```bash
sessionbridge agent whoami --context <CONTEXT> --format json
sessionbridge agent next --context <CONTEXT> --format json
sessionbridge agent candidates --context <CONTEXT> --format json
sessionbridge item propose --claim --input-json work-item.json \
  --context <CONTEXT> --expected-plan-version <N> --format json
```

`item propose --claim` is the fast path for newly discovered executable work.
Under `bounded` or `open` autonomy it applies the PlanChange and creates the
active WorkItem, execute Claim, Attempt, and Reservations in one transaction.
Any Claim validation or resource conflict rolls the complete proposal back.

The protocol distinguishes these outcomes:

- `await`: all executable candidates are blocked by one or more WorkItems
  currently owned by other Sessions. `agent await work-items` blocks locally
  without a model call until those WorkItems are accepted or become impossible.
- `blocked`: the Agent believes work remains but cannot safely advance it.
- `no_further_work`: after a current global scan, the Agent cannot find more
  work that should be advanced.

Each worker decides its own outcome. The task completes only after every
non-retired worker has an explicit `no_further_work` conclusion and all
blocking invitation seats have been consumed, revoked, abandoned, or expired.

Dependency-aware waiting has no wall-clock timeout:

```bash
sessionbridge agent await work-items --item <WORK_ITEM_ID> \
  --context <CONTEXT> --request-id <REQUEST_ID> --format json

sessionbridge agent await work-items --mode all \
  --item <WORK_ITEM_ID_1> --item <WORK_ITEM_ID_2> \
  --context <CONTEXT> --request-id <REQUEST_ID> --format json
```

The command writes no periodic stdout. WorkItem acceptance, cancellation,
supersession, or Task termination produces one structured callback and restores
the waiting Participant to `active`. Interrupted hosts resume the same durable
wait with `sessionbridge agent await resume <WAIT_ID>`.

### Task Workgroup Chat

Each Task has an auditable workgroup chat. Agents use it to expose meaningful
intent, progress, questions, decisions, risks, and handoffs:

```bash
sessionbridge agent chat send --context <CONTEXT> --input message.json
sessionbridge agent chat inbox --context <CONTEXT> --limit 20
sessionbridge agent chat ack --context <CONTEXT> --message <MESSAGE_ID>
sessionbridge agent chat await --context <CONTEXT>
sessionbridge agent chat history --context <CONTEXT> --since <EVENT_CURSOR> --limit 100
```

Example `message.json`:

```json
{
  "kind": "progress",
  "body": "The storage migration is complete; I am validating reconnect behavior.",
  "reply_to_message_id": null,
  "mentions": [],
  "related_entities": [
    { "entity_type": "work_item", "entity_id": "<WORK_ITEM_ID>" }
  ]
}
```

The same operations are available through `sessionbridge_chat_send`,
`sessionbridge_chat_inbox`, `sessionbridge_chat_ack`, `sessionbridge_chat_await`, and
`sessionbridge_chat_history`. Each Agent Session has an independent unread
inbox. Mentions and replies to that Session are delivered before broadcasts;
reading does not acknowledge messages, and ACK accepts the exact processed
message IDs. Messages sent before a Session joined remain available in history
without entering its unread inbox. `chat await` blocks locally with no periodic
model calls and returns when directed unread chat arrives; `--include-broadcasts`
also wakes for ordinary group messages. The desktop GUI exposes the shared conversation
in its **群聊** view, where the operator can read and reply.

Chat is informational. It does not replace Claims, approvals, Deliverables,
Evidence, handoffs, or blocking CoordinationRequests, and Agents should post
state changes rather than narrating every command.

### Deliverable Completion Contract

Tasks can define user-visible deliverables that must be mapped to work and verified by passed evidence before autonomous completion is allowed:

```bash
sessionbridge deliverable add current --key cli-run --title "Expose the run command"
sessionbridge deliverable map current <DELIVERABLE_ID> <WORK_ITEM_ID>
sessionbridge deliverable verify current <DELIVERABLE_ID> --evidence <PASSED_EVIDENCE_ID>
sessionbridge deliverable list current --format json
```

`agent sync --full --format json` includes the complete deliverable list. Required deliverables that are open, unmapped, or unverified are exposed in Task health and block `task.completed`, even when every worker has concluded.

When an Agent reports a ChangeSet as integrated, SessionBridge verifies that the Task target branch actually contains the ChangeSet head commit. The Agent performs the merge or cherry-pick and retries `changeset integrate`; SessionBridge does not mutate the target branch.

### Compact Agent Runtime

The default Agent read path returns a compact, advisory action snapshot instead of the complete Task graph:

```bash
sessionbridge agent next --context <CONTEXT> --format json
sessionbridge agent next --context <CONTEXT> --preview --format json
sessionbridge agent operations --format json
```

Use `agent sync --full` only when the Agent needs the complete criteria, Deliverable, participant, invitation and event history. Planner candidates are explicitly non-exhaustive; an Agent can preserve a newly discovered action without executing unknown side effects:

```bash
sessionbridge agent propose-action \
  --context <CONTEXT> \
  --kind alternative_validation \
  --input proposal.json
```

`agent next` returns scoped state tokens, exact awaitable dependency ownership,
soft scheduling recommendations, and a stable Operation Contract reference.
`--preview` performs the same read without updating the Session heartbeat.
`agent conclude` acquires the current completion scope internally unless an
advanced caller supplies `--completion-token` explicitly:

```bash
sessionbridge agent conclude \
  --outcome no-further-work \
  --summary "No further work remains after the global scan" \
  --checked "requirements, repository, tests, review and integration"
```

Use `sessionbridge agent operations <OPERATION> --format json` to obtain the
required flags, JSON Schema, template, invariants, and executable example for a
high-level operation. Existing commits ahead of the Task target branch must be
adopted explicitly with `work adopt`; routine `work start` refuses to hide them
inside the Attempt baseline.

## State Layout

The initialized checkout and worktrees registered with `workspace link` contain
the discoverable project configuration:

```text
<worktree>/.sessionbridge/config.toml
```

Automatically provisioned Agent worktrees are addressed by the private context
returned from `agent join`; Agent commands in those worktrees must keep passing
`--context <CONTEXT>`, as shown in the generated prompt and `next_actions`.

Mutable coordination state is outside the worktree:

```text
$XDG_STATE_HOME/sessionbridge/
  workspaces/<project-id>/<repository-instance-id>/
    state.db
    migration.lock
  contexts/<session-id>.json
  invites/
  prompts/
  worktrees/
  join-attempts/
  launches/<workspace-id>/<repository-instance-id>/<launch-id>.json
```

All Git worktrees sharing one Git common directory share a repository instance
ID and database. Independent clones are isolated by default. A non-Git
directory gets an instance ID bound to its canonical path and only supports
advisory coordination; automatic Git worktree provisioning is unavailable.
If Git is initialized after SessionBridge, discovery inherits the existing
directory instance ID so the original Task and Invitation database remains
authoritative. `sessionbridge doctor` reports `GIT_HEAD` as failed until the
repository has an initial commit, and worker invitation creation is rejected
before any invitation is written.

An existing Git worktree can be linked and then registered during join:

```bash
sessionbridge workspace link --source /repo/main --path /repo/agent-worktree
sessionbridge agent join --invite-file invite.json --worktree /repo/agent-worktree
```

`workspace link` verifies the Git common directory and adds `.sessionbridge/`
to Git's local exclude so coordination metadata does not dirty the worktree.

## Installation

The project currently targets Rust 1.95 and Edition 2024:

```bash
git clone git@github.com:GTJasonMK/sessionBridge.git
cd sessionBridge
cargo build --release
cargo install --path .
```

Runtime requirements:

- Git for repository and automatic worktree operations.
- A local filesystem suitable for SQLite WAL. Network filesystems are not
  supported by the direct-SQLite architecture.

## Desktop GUI

SessionBridge includes a Tauri 2 operator console for publishing Tasks,
copying Agent prompts, monitoring autonomous work, and inspecting diagnostics.
The GUI calls the same Rust application services as the CLI and never parses
CLI text output.

When `sessionbridge-gui` is installed beside the CLI or available on `PATH`,
launch it from any directory with:

```bash
sessionbridge gui
```

The command starts the desktop process in the background and returns its PID.
Use `sessionbridge gui --foreground` when terminal-attached diagnostics are
needed. `SESSIONBRIDGE_GUI=/absolute/path/sessionbridge-gui` overrides binary
discovery.

Linux development requirements include Node.js 20+, WebKitGTK 4.1, and GTK 3.

```bash
cd gui
npm ci
npm run tauri dev
```

Build the Linux AppImage and deb packages with:

```bash
cd gui
npm run tauri build
```

For a local production binary without installer packages:

```bash
cd gui
npm run tauri build -- --no-bundle
```

Do not use a plain `cargo build` of `sessionbridge-gui` as the distributable
desktop build. That path enables Tauri's development configuration and expects
the Vite server at `127.0.0.1:1420`; the Tauri CLI build embeds `gui/dist` and
enables the production custom protocol.

On first launch, select a repository that already contains
`.sessionbridge/config.toml`. The application remembers the last valid
Workspace. Agent sessions remain in external coding tools; use the **Agent
Prompts** action to view and copy one prompt per invitation seat.

## Machine Protocol

Use `--format json` or `--format jsonl` for Agent integrations. Standard output
contains data only; diagnostics and errors go to standard error. A successful
JSON command uses this envelope:

```json
{
  "schema_version": "1",
  "ok": true,
  "data": {},
  "meta": {
    "timestamp": "2026-07-10T12:00:00Z",
    "cursor": 1843
  }
}
```

Errors contain a stable uppercase code, a retryable flag, and structured
details. IDs are UUIDv7 strings, timestamps are UTC RFC 3339, and event cursors
are monotonically increasing SQLite integers. See
[`docs/protocol.md`](docs/protocol.md) for the command and state contracts.

## Operational Logs

SessionBridge writes operational diagnostics as structured JSONL without
mixing them into command stdout or stderr. The default directory is the
`logs/` child of the SessionBridge user state directory. It is private (`0700`
on Unix), rotates daily in UTC, and retains at most 15 log files.

```bash
sessionbridge logs path --format json
sessionbridge logs tail --lines 200 --format json
sessionbridge diagnostics collaboration current --format json
```

Every successful or failed JSON response includes `meta.invocation_id`. The
same ID appears on the command start, completion, and failure records, so a
reported error can be located exactly:

```bash
sessionbridge logs tail --lines 1000 --format json | rg '<INVOCATION_ID>'
```

The default filter is `sessionbridge=info`. Enable transaction, audit-event,
Git, context, and database diagnostics for reproduction runs with:

```bash
SESSIONBRIDGE_LOG=sessionbridge=debug sessionbridge doctor --format json
```

`SESSIONBRIDGE_LOG_DIR` overrides the directory, and `RUST_LOG` is accepted
when `SESSIONBRIDGE_LOG` is unset. Logs record exact subcommand names,
Workspace/Task/Session IDs when available, paths, request/invocation IDs,
durations, error codes, and Git argv, but never serialize CLI input bodies,
invitation tokens, Session tokens, or context file contents. SQLite Events are
the durable business audit trail; JSONL files are operational diagnostics.

## Security Model

SessionBridge 0.1 assumes all Agents run under one trusted OS user. It reduces
accidental credential exposure but is not an isolation boundary between
processes owned by that user.

- Invitation and Session tokens use the OS CSPRNG; SQLite stores only SHA-256
  hashes.
- Invitation, context, prompt, staging, and recovery artifacts use private
  permissions (`0600` files and `0700` directories on Unix).
- Tokens are not placed in prompt text, argv, event payloads, JSON manifests,
  tracing, or normal errors.
- A prompt never carries a reusable join request ID. Each Agent creates its own
  join attempt, so a prompt accidentally sent twice cannot replay another
  Agent's Session result.
- SQL uses parameters, and Git is invoked with fixed argv rather than shell
  command construction.

## Development

The repository is organized around one Rust domain/application core shared by
the CLI, MCP server, and Tauri GUI:

```text
src/app/               application services and reconciliation
src/domain/            state machines, IDs, and domain invariants
src/storage/           SQLite store and append-only migrations
src/invitation/        invitation secrets, files, and prompt rendering
tests/                 cross-service and CLI contract tests
gui/src/               React operator console
gui/src-tauri/         Tauri commands and desktop packaging
docs/                  protocol and architecture decisions
```

The detailed Agent runtime design is documented in
[Agent Runtime Optimization Plan](docs/AGENT_RUNTIME_OPTIMIZATION_PLAN.md).

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

GitHub Actions runs the same Rust, frontend, and Tauri Linux release checks on
pushes and pull requests. Product rationale and the historical delivery plan
are in [`docs/PROJECT_PLAN.md`](docs/PROJECT_PLAN.md).

Read [CONTRIBUTING.md](CONTRIBUTING.md) before changing business invariants,
schema, machine contracts, or security boundaries. Report vulnerabilities
privately according to [SECURITY.md](SECURITY.md).

## License

SessionBridge is dual-licensed under the Apache License 2.0 or the MIT License,
at your option. See [LICENSE-APACHE](LICENSE-APACHE) and
[LICENSE-MIT](LICENSE-MIT).
