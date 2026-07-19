# SessionBridge Protocol

> Status: implemented protocol  
> Output schema version: `1`  
> Coordination protocol version: `1`  
> Prompt template version: `1`

This document is the machine-facing source of truth for SessionBridge 0.1.
Product rationale and design tradeoffs remain in `PROJECT_PLAN.md`.

## 1. General Contract

- IDs are lowercase UUIDv7 strings.
- Database timestamps are signed Unix microseconds.
- JSON timestamps are UTC RFC 3339 strings.
- Database enum values use `snake_case`.
- CLI enum values use `kebab-case` where a value contains multiple words.
- JSON and JSONL output never contains ANSI control sequences or prompts.
- Diagnostics go to stderr. Successful data goes to stdout.
- Text output is reserved for direct-send prompts and explicitly human output.
- Every write accepts a request ID. Reusing a request ID with a different
  canonical payload returns `IDEMPOTENCY_KEY_REUSED`.

### 1.1 Success Envelope

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

`meta.cursor` is omitted when the command does not establish an event cursor.

### 1.2 Error Envelope

```json
{
  "schema_version": "1",
  "ok": false,
  "error": {
    "code": "VERSION_CONFLICT",
    "message": "task version changed",
    "retryable": false,
    "details": {
      "expected_version": 4,
      "actual_version": 5
    }
  },
  "meta": {
    "timestamp": "2026-07-10T12:00:00Z"
  }
}
```

Exit codes:

| Exit | Meaning |
|---:|---|
| `0` | Success |
| `1` | Domain, storage, or internal failure |
| `2` | Invalid input or CLI usage |
| `3` | Workspace/entity/invitation not found |
| `75` | Transient SQLite busy/locked condition |

## 2. Stable Error Codes

The current stable codes are:

```text
INVALID_INPUT
NOT_FOUND
WORKSPACE_NOT_FOUND
CURRENT_TASK_AMBIGUOUS
LAUNCH_NOT_FOUND
LAUNCH_BASELINE_CHANGED
LAUNCH_RECOVERY_REQUIRED
TASK_NOT_ACTIVE
TASK_PAUSED
PLAN_CHANGED
VERSION_CONFLICT
ITEM_NOT_READY
CAPABILITY_MISMATCH
CAPABILITY_NOT_GRANTED
SESSION_CAPACITY_REACHED
CLAIM_CONFLICT
CLAIM_EXPIRED
STALE_CLAIM
RESOURCE_CONFLICT
WORKTREE_CONFLICT
WORKTREE_DIRTY
SCOPE_DRIFT
OPEN_BLOCKER
INPUT_INVALIDATED
CONTRACT_NOT_ACCEPTED
PROPOSAL_REVIEW_REQUIRED
APPROVAL_REQUIRED
DECISION_REQUIRED
CONCLUSION_STALE
COMPLETION_EPOCH_CHANGED
PARTICIPANT_VERSION_CHANGED
SESSION_HAS_ACTIVE_WORK
SESSION_ALREADY_CONCLUDED
COMPLETION_COHORT_OPEN
PARTICIPANT_BLOCKED
PARTICIPANT_STALE
PARTICIPANT_DEPARTED
PARTICIPANT_RETIRED
RETIREMENT_NOT_ALLOWED
WAIT_NOT_ALLOWED
INVITE_NOT_FOUND
INVITE_CONSUMED
INVITE_REVOKED
INVITE_EXPIRED
INVITE_RESERVED
INVITE_NOT_SENDABLE
INVITE_SECRET_MISSING
INVITE_OUTPUT_REQUIRED
JOIN_RECOVERY_REQUIRED
SECRET_OUTPUT_NOT_ALLOWED
PROTOCOL_VERSION_MISMATCH
PROMPT_TEMPLATE_UNSUPPORTED
IDEMPOTENCY_KEY_REUSED
DATABASE_BUSY
MIGRATION_FAILED
SCHEMA_UPGRADE_REQUIRED
SCHEMA_TOO_NEW
CURSOR_EXPIRED
IO
DATABASE
INTERNAL
```

Clients must branch on `code`, not localized or evolving message text.

## 3. Workspace Discovery

Precedence:

```text
--context / --workspace
SESSIONBRIDGE_CONTEXT / SESSIONBRIDGE_WORKSPACE
upward discovery of .sessionbridge/config.toml
```

Project config schema:

```toml
schema_version = 1
protocol_version = 1
project_id = "019..."
name = "SessionBridge"
```

The mutable database is selected by `(project_id,
repository_instance_id)`. Opening it verifies the `workspaces` row before any
task operation.

When a SessionBridge directory workspace is converted in place with `git
init`, the Git common directory inherits the existing directory instance ID.
This preserves the database binding instead of creating a second empty state
location. If both identities already own databases, discovery fails with
`INVALID_INPUT` and `recovery_required: true`; it never chooses one database
silently.

Worker invitation prompts require automatic worktree provisioning. Invitation
creation therefore requires a Git repository whose `HEAD` resolves to an
initial commit. This precondition is reported by `doctor` as `GIT_HEAD` and is
checked before invitation records or prompt files are created. Observer
invitations do not request an isolated worktree.

## 4. Task Contract

`task publish` atomically creates:

- one active Task;
- the user Objective;
- optional Constraint, Non-goal, and Done-condition criteria;
- five system completion checklist criteria when no Done conditions were
  supplied;
- one `task.published` Event;
- an optional idempotency record.

Initial task counters:

```json
{
  "phase": "active",
  "plan_version": 0,
  "completion_epoch": 1,
  "completion_revision": 0,
  "version": 0
}
```

Lifecycle transitions:

```text
active -> paused -> active
active | paused -> cancelled
completed -> active       # explicit reopen; new completion epoch
completed | cancelled -> archived
```

Task health contains mechanical completion signals. These signals inform Agent
judgment; they are not a semantic proof of task completion.

### 4.1 Recoverable Launch

`launch` is the human-facing composition of `task publish` and one worker
Invitation batch. It does not replace either lower-level protocol.

```bash
sessionbridge launch --brief task.md --agents 4 --expires-in 2h
sessionbridge launch resume <LAUNCH_ID>
```

The complete brief becomes one immutable Objective. `--title` overrides title
derivation; otherwise the first H1, file stem, or first non-empty direct-text
line is used. Built-in defaults are four workers, two hours, `bounded`, POSIX
Prompt rendering, and the current Git branch.

Launch preflight requires a registered workspace, a readable current database,
a Git repository with a resolvable HEAD and target branch, and a clean baseline.
`--allow-dirty-baseline` records a warning and the paths absent from Agent
worktrees. `--dry-run` writes no marker, Task, Invitation, Event, or Prompt.

Private recovery state is stored at:

```text
$XDG_STATE_HOME/sessionbridge/launches/<workspace-id>/<repository-instance-id>/<launch-id>.json
```

The marker contains a canonical request, baseline commit, phase, Task ID,
batch ID, and two different child request IDs. It never contains Invitation or
Session tokens. Task publication and Invitation creation retain their existing
idempotency protocols. Replaying the same launch ID and payload returns the
same Task and batch; a different payload returns `IDEMPOTENCY_KEY_REUSED`.

If a failure occurs after Task publication, `LAUNCH_RECOVERY_REQUIRED` includes
the launch ID, Task ID, cause and resume argv. A moved target branch returns
`LAUNCH_BASELINE_CHANGED`; only `launch resume --accept-baseline-change` may
adopt the new commit before Invitation completion.

Task references accept a UUID or `current`. `current` resolves only when
exactly one Task is `active` or `paused`; multiple candidates return
`CURRENT_TASK_AMBIGUOUS` with their IDs and titles. Top-level `status` combines
Task details and health, while top-level `watch` delegates to the existing Task
watch/reconciliation loop.

`prompt list/show` resolve persisted Invitation batches. Indexes are the
stored 1-based batch indexes and never shift when another Invitation changes
state. Text `prompt show` emits the sendable direct prompt; `--audit` may render
historical non-sendable prompts without exposing token material.

### 4.2 Project Launch Settings

Optional `.sessionbridge/settings.toml` schema 1 stores `default_profile` and
named profiles with `agents`, `expires_in`, `autonomy`, `shell`, and optional
`target_branch`. CLI flags override the selected profile, which overrides
built-in defaults. Dirty-baseline permission is never persisted. Unknown
fields, profiles, schemas, or invalid values are errors.

`config init` creates the file only when absent; `config show` and `config
validate` never modify it. Interactive launch is available only for human
formats on a real TTY, asks for missing values, displays a final summary, and
defaults confirmation to No.

## 5. Invitation Protocol

### 5.1 Default File Delivery

Each invitation has a different 256-bit token. SQLite stores the SHA-256 token
hash. The direct-send prompt contains a private invitation file path, never the
token.

Public manifest:

```json
{
  "invite_id": "019...",
  "task_id": "019...",
  "batch_id": "019...",
  "batch_index": 1,
  "batch_count": 4,
  "status": "pending",
  "participant_kind": "worker",
  "blocks_completion": true,
  "completion_epoch": 1,
  "expires_at": "2026-07-10T13:00:00Z",
  "prompt_template_version": 1,
  "protocol_version": 1,
  "prompt_path": "/private/.../prompt-019....txt",
  "prompt_sha256": "...",
  "secret_delivery": {
    "kind": "file",
    "invite_path": "/private/.../invite-019....json"
  },
  "secret_exposed": false,
  "sendable": true
}
```

Creation is a recoverable two-phase file/database protocol:

1. Build every secret, prompt, and request marker in a private staging
   directory and fsync each file.
2. `BEGIN IMMEDIATE`: insert the complete batch as `prepared` and increment the
   Task completion revision.
3. Atomically rename the staging directory.
4. `BEGIN IMMEDIATE`: change the complete batch to `pending`, append Events,
   and store the idempotent result.

Invitation state:

```text
prepared -> pending -> reserved -> consumed
prepared -> abandoned
pending -> revoked | expired
reserved -> pending | revoked | expired
```

Worker invitations always block completion. Observer invitations never do.

### 5.2 Prompt Safety

The prompt embeds stable protocol instructions, Task ID, invitation ID, expiry,
and the invitation path. It does not embed task prose, token material, or a
join request ID. A second Agent receiving the same prompt therefore creates a
different join request and receives `INVITE_RESERVED`, `INVITE_CONSUMED`, or
`INVITE_SECRET_MISSING`; it cannot replay the first Agent's Session result.

### 5.3 Explicit Inline Delivery

Inline delivery is an opt-in exception for a trusted single-recipient channel:

```bash
sessionbridge agent invite create --task <TASK_ID> \
  --secret-delivery inline --allow-secret-output --format text
```

It is accepted only for one invitation and text output. Batch, JSON, JSONL,
and table output return `SECRET_OUTPUT_NOT_ALLOWED`. The persisted invitation
still uses the same one-time token hash, expiry, reservation, and recovery
protocol; only the rendered stdout prompt contains the secret.

## 6. Join Protocol

Join uses a reservation plus filesystem commit protocol:

Unlike general idempotency keys, an explicit join `--request-id` must be a UUID.
When omitted, the CLI generates a fresh local UUID and persists it in the join
attempt marker.

1. Generate a local join request ID, Session ID, Participant ID, and private
   attempt marker.
2. `BEGIN IMMEDIATE`: validate token hash, task phase, epoch, expiry, and
   invitation state; conditionally set `reserved_by_request_id` and a lease.
3. Provision a unique Git worktree when requested, or validate and register an
   existing clean worktree passed through `--worktree`.
4. Generate a Session token and write a private temporary context.
5. `BEGIN IMMEDIATE`: create AgentSession and TaskParticipant, consume the
   invitation, increment completion revision, append Events, and store the
   idempotent result.
6. Atomically rename the temporary context and remove the one-time invitation
   secret and attempt marker.

Join result never returns the Session token:

```json
{
  "session_id": "019...",
  "participant_id": "019...",
  "participant_kind": "worker",
  "completion_epoch": 1,
  "context_path": "/private/contexts/019....json",
  "worktree_path": "/private/worktrees/019...",
  "task_phase": "active",
  "protocol_version": "1",
  "prompt_template_version": "1",
  "event_cursor": 1843,
  "next_actions": [
    {
      "cwd": "/private/worktrees/019...",
      "argv": ["sessionbridge", "agent", "whoami", "--context", "..."]
    }
  ]
}
```

`JOIN_RECOVERY_REQUIRED` details contain the local marker, request ID, and
retry argv. Only that request ID may recover the reserved attempt.

An explicit worktree must resolve to the same Git common directory as the
Workspace. `workspace link --source <existing-workspace> --path <worktree>`
copies the stable project config, shares the existing state database, and adds
`.sessionbridge/` to Git's local exclude.

## 7. Session Context and Authentication

Private context schema:

```json
{
  "schema_version": 1,
  "protocol_version": 1,
  "workspace_id": "019...",
  "repository_instance_id": "019...",
  "task_id": "019...",
  "session_id": "019...",
  "token": "sb_session_<secret>",
  "database_path": "/absolute/path/state.db"
}
```

Every Agent command verifies:

- context schema and protocol version;
- absolute database path;
- workspace and repository instance binding;
- Session/Task binding;
- constant-work token hash comparison;
- current TaskParticipant record.

`agent join --nickname <NAME>` records a normalized 1-32 character Session
nickname. Chat sender labels, participant views, diagnostics, and audit output
all read this canonical Session label; nickname uniqueness is not used as an
identity or authorization boundary.

The context object has a redacted Debug implementation and is never included in
normal command output.

## 8. Agent Sync

`agent sync --full` returns one current global view. Compact runtime reads use
`agent next`; `agent sync` without `--full` is rejected rather than silently
changing operation semantics.

- whoami Session and participant projection;
- Task and all TaskCriterion records;
- Task health and completion signals;
- ready candidate WorkItems with capability match reasons;
- open Gaps;
- current participants and derived stale markers;
- invitation seats;
- directed or capability-matching inbox requests;
- Events after an optional cursor.

Event results are ordered by `sequence`. The current response limit is 1000;
`has_more_events=true` instructs the client to continue from `next_cursor`.

## 9. Event Cursor

Events are immutable and use `INTEGER PRIMARY KEY AUTOINCREMENT`. Current event
types include:

```text
workspace.initialized
task.published
invitation.created
invitation.reserved
invitation.consumed
invitation.revoked
invitation.rotated
invitation.expired
agent.joined
agent.waiting
agent.await_registered
agent.await_target_satisfied
agent.await_satisfied
agent.await_dependency_failed
agent.await_task_terminal
agent.resumed
agent.concluded
agent.blocked
agent.left
requirement.created
gap.reported
blocker.opened
blocker.resolved
plan.applied
work.proposed
work.split
work.related
work.reopened
claim.acquired
claim.renewed
claim.released
claim.yielded
claim.abandoned
claim.reclaimed
attempt.checkpointed
item.submitted
review.completed
changeset.integration_completed
agent.intent_set
agent.intent_cleared
coordination.requested
coordination.responded
contract.published
contract.revised
contract.accepted
decision.proposed
decision.resolved
invalidation.resolved
participant.retired
capability.attested
plan_change.resolved
task.completed
```

Cursor is a synchronization and audit mechanism. It does not replace Task
`completion_revision`, entity versions, Claim generation, or fencing tokens.

## 9.1 Operational Diagnostics

The immutable SQLite Event table is the authoritative business audit trail.
Separately, each CLI process writes structured JSONL operational diagnostics to
the private user-state log directory. Log files rotate daily in UTC and retain
at most 15 files. They cover command start/completion/failure, elapsed time,
stable error code, request and invocation correlation, database open/migration,
Git command timing, context loading, and audit Event insertion metadata.

All normal JSON responses and structured error responses include
`meta.invocation_id`; the same value is attached to every log record for that
process. `logs path` returns the effective directory and rotation policy, while
`logs tail --lines N` returns parsed recent records. `doctor` verifies that the
directory exists and is private.

The default filter is `sessionbridge=info`. `SESSIONBRIDGE_LOG` overrides it,
then `RUST_LOG`; `SESSIONBRIDGE_LOG_DIR` overrides the directory. Operational
logging never records invitation or Session tokens, context contents, stdin,
JSON input bodies, prompt bodies, or Event payload JSON. It may record local
paths, entity IDs, request IDs, error text, and fixed Git argv. Logs never write
to stdout and therefore cannot corrupt the machine protocol.

## 10. Completion CAS

An Agent conclusion is based on:

```text
task_id
completion_epoch
basis_completion_revision
basis_event_cursor
basis_plan_version
```

Only the completion revision and epoch provide the stop-decision CAS. The event
cursor records what the Agent observed. Earlier `no_further_work` conclusions
remain valid participant exit records when other Agents subsequently add normal
work; the final active Agent performs the latest global scan.

The CLI normally acquires this scoped basis internally from `agent next`, so a
typed conclusion only needs `--outcome`, `--summary`, and one or more
`--checked` values. `--assessment-file` and `--completion-token` remain advanced
escape hatches. Repeating the same terminal outcome for an already concluded or
blocked Participant returns the existing immutable conclusion.

Mechanical completion requires:

- at least one non-retired worker in the current epoch;
- every non-retired worker has a current `no_further_work` conclusion;
- no active, waiting, blocked, departed, or stale worker;
- no open blocking worker invitation;
- no participant-owned active Claim, Intent, unsubmitted Attempt, or writable
  Reservation;
- Task phase is still `active`.

The reducer stores an immutable TaskCompletion, conclusion links, a final signal
snapshot, and any assurance flags before changing Task phase to `completed`.

## 11. Planning and Execution

Planning writes use `--expected-plan-version`. Under `bounded` and `open`, safe
changes apply immediately. Under `proposal`, entities remain `proposed` and a
pending PlanChange is created; `plan approve` applies it with a peer approval,
or with a recorded single-worker fallback when no peer exists.

Core WorkItem proposal input:

```json
{
  "kind": "implementation",
  "title": "Implement claim fencing",
  "description": "",
  "required": true,
  "priority": 10,
  "risk": "medium",
  "required_capabilities": ["rust"],
  "acceptance_criteria": ["stale generations are rejected"],
  "expected_artifacts": ["source", "tests"],
  "planned_resources": ["path:src/app/work.rs"],
  "parent_id": null,
  "dedupe_key": "claim-fencing",
  "requirement_ids": [],
  "depends_on": [],
  "base_plan_version": 4,
  "reason": "Create independently claimable work"
}
```

`item claim` requires `--expected-version` and `--observed-plan-version`.
`resources-file` is an array:

```json
[
  {
    "resource_key": "path:src/app/work.rs",
    "mode": "write",
    "policy": "enforced",
    "pinned_revision": null
  }
]
```

`item propose --claim` accepts the same WorkItem proposal input and is available
only when the Task autonomy policy applies the proposal immediately (`bounded`
or `open`). A single `BEGIN IMMEDIATE` applies the PlanChange, advances the plan
version and completion revision once, creates the active WorkItem, and acquires
its execute Claim with an Attempt and Reservations. Claim readiness, capability,
dependency, worktree, or resource validation failure rolls back the WorkItem,
coverage, relations, PlanChange, version updates, events, and Claim together.
The command records one idempotency result; retrying the same request ID and
payload returns the original WorkItem, Claim, and Attempt. Under `proposal`, it
returns `PROPOSAL_REVIEW_REQUIRED` instead of creating an unclaimable proposed
entity.

Before a Git-backed execute Claim is inserted, the registered worktree must be
readable, clean, attached to the registered branch, and at a resolvable HEAD.
The Attempt base is the current Task target-branch HEAD, not an arbitrary
worktree HEAD. If the worktree already contains clean commits ahead of that
target, `work start` returns `WORKTREE_CONFLICT`; the Agent must either rebase a
diverged branch or explicitly use `work adopt` to associate those commits with
the new Attempt. A Git-backed submit never falls back to the Agent-provided
changed-resource list when the registered repository cannot be opened.

Claim acquisition atomically creates WorkClaim, WorkAttempt, input pins, and
all Reservations. Every later canonical write carries Claim ID and generation.
Expired generations cannot renew or submit; `claim reclaim` closes the old
Claim/Attempt and creates the next generation.

`claim scope reconcile <CLAIM> --fence <GENERATION> --from-git` derives one
batch from the Attempt base-to-HEAD diff plus current dirty paths. It only adds
uncovered writable resources, validates all conflicts in one transaction, and
never silently removes scope. This replaces repeated one-path-at-a-time scope
expansion.

`attempt checkpoint --input-json` accepts:

```json
{
  "summary": "implementation ready",
  "changed_resources": ["path:src/app/work.rs"],
  "artifacts": [],
  "evidence": []
}
```

`item submit` reads Evidence and ChangeSet files. In a Git worktree, changed
resources are computed from the actual `base_commit..head_commit` diff; the
Agent-provided list cannot override it. Submit rejects scope drift, closes the
execute Claim, transfers the writable conflict footprint to the ChangeSet,
and moves the WorkItem to `submitted`. Only an approved ReviewResult followed
by successful ChangeSet integration moves it to `accepted`.

## 12. Collaboration and Governance

### Task workgroup chat

Each Task owns an auditable workgroup chat. Agent messages use:

```bash
sessionbridge agent chat send --context <CONTEXT> --input message.json
sessionbridge agent chat inbox --context <CONTEXT> --limit 20
sessionbridge agent chat ack --context <CONTEXT> --message <MESSAGE_ID>
sessionbridge agent chat await --context <CONTEXT>
sessionbridge agent chat history --context <CONTEXT> --since <EVENT_CURSOR> --limit 100
```

Equivalent MCP tools are `sessionbridge_chat_send`, `sessionbridge_chat_inbox`,
`sessionbridge_chat_ack`, `sessionbridge_chat_await`, and `sessionbridge_chat_history`.
Message kinds are `progress`, `intent`, `question`, `decision`, `warning`, `handoff`, and
`note`. Messages may reply to an earlier message, mention Task Session IDs, and reference
domain entities. The GUI operator can participate as `operator`.

`chat_messages.event_sequence` references the immutable `chat.message_sent` Event created in
the same SQLite transaction, so chat history and domain events share one recovery cursor.
Request IDs make sends idempotent. Reply targets and mentions must belong to the same Task.
Each Session has an independent unread inbox whose start sequence is captured atomically when
the Session is created. Messages from before that point remain in history but are not unread.
Messages that mention the Session, or reply to a message sent by it, are selected ahead of
broadcasts. Reading is side-effect free. The Agent acknowledges only the exact message IDs it
has processed; per-message receipts make retries idempotent without incorrectly skipping
unreturned messages between Event cursors. An Agent's own messages never enter its inbox.
`chat await` blocks in the local CLI/MCP process without repeatedly invoking the model. It
returns when directed unread chat arrives by default, can include broadcasts explicitly, and
terminates if the Task or Participant becomes terminal. Because unread state is durable, a
disconnected caller safely invokes it again without a separate wait record.
Chat is informational: it cannot create Claims, satisfy approvals, change completion state, or
replace a blocking CoordinationRequest. Agents should post important intent, progress, risks,
decisions, questions, and handoffs without narrating every mechanical command.

### Runtime reconciliation and operation contracts

`task reconcile <TASK>` is the single cross-domain reducer for external Git
truth and mechanically derived state. It records target branch observations,
detects when a historically integrated ChangeSet is no longer an ancestor of
the target branch, opens a system-managed Invalidation, downgrades affected
Requirement coverage and Deliverables, and reverses those derived changes when
ancestry is restored. System-managed integrity Invalidations cannot be manually
resolved while drift persists. `task integrity <TASK>` returns the persisted
observation.

The same reducer advances WorkItem-backed Requirement coverage and verifies or
invalidates Deliverables after Evidence, submit, review, and integration
changes. Every derived transition is an immutable Event and completion revision
change; Evidence history is retained.

`agent operations [OPERATION]` exposes executable Operation Contracts containing
required and optional fields, JSON Schema, templates, hard invariants, and
examples. MCP exposes typed `sessionbridge_agent_next` and
`sessionbridge_agent_conclude` tools in addition to durable wait and chat tools.
`agent next --preview` and MCP `preview=true` do not update heartbeat.

Participant inactivity is advisory at 10 minutes. At 30 minutes it becomes an
Agent-managed retirement candidate; SessionBridge never auto-retires it.
Planner soft recommendations distribute known ready work deterministically but
remain advisory and do not reserve or hide any WorkItem.

`diagnostics collaboration <TASK>` returns structural findings for Git drift,
orphan waits, all-worker waiting states, unowned review or integration stages,
and stale or retirement-candidate Participants. The GUI diagnostic page calls
the same Rust analysis use case.

AgentIntent is a TTL-bound soft declaration. `plan` and `implement` intents
block that participant from concluding until cleared. CoordinationRequest can
target a Session or capability; an assigned blocking request permits bounded
durable `agent await` and remains in the inbox until resolved or cancelled.

`agent await work-items` is a separate durable dependency primitive. It accepts
one or more WorkItem IDs with `all` or `any` semantics and has no time-based
termination. The invoking CLI process blocks locally and refreshes only the
Session heartbeat; it does not repeatedly invoke `agent sync` or the model.
Successful ChangeSet integration moves the WorkItem to `accepted` and satisfies
matching Wait targets in the same SQLite transaction. Cancelled or superseded
WorkItems return `dependency_failed`; Task cancellation or archival returns
`task_terminal`. Every terminal result restores the Participant from `waiting`
to `active` and includes the triggering Event cursor and authoritative result
IDs. Polling-based `agent wait` has been removed; all dependency waits use durable await records.

Contract publication creates revision 1 with a SHA-256 digest. Revisions are
immutable rows; accepting a revision supersedes other published revisions.
Changing a Contract creates Invalidations for pinned WorkItems. Decisions and
PlanChanges use Approval rows; a proposer cannot self-approve while another
active worker is available.

`participant retire` is not self-service. It requires another active worker,
a stale, blocked, or departed target with no Claims, Intents, or Reservations,
and a JSON handoff. Retirement is audited and the completion reducer runs in
the same transaction.

Top-level `task criterion add/edit` is a user-goal change. It increments plan
version and completion epoch, supersedes the old participant cohort, migrates
only active/waiting worker Sessions, and rebinds unexpired open invitations.
Old AgentConclusions remain immutable history and cannot complete the new
epoch.

## 13. SQLite and Migration

Every connection enforces and verifies:

```sql
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
PRAGMA synchronous = FULL;
```

Busy timeout is five seconds. Normal stores hold a shared migration file lock;
`migrate` holds the exclusive lock. Opening never performs an implicit
migration. An existing older database is backed up and integrity-checked before
upgrade. A newer schema returns `SCHEMA_TOO_NEW`.

## 14. Compatibility Rules

- Unknown enum values are errors; clients must not coerce them.
- Unknown JSON object fields should be ignored by readers unless a command
  explicitly declares a closed schema.
- Missing required fields are errors.
- A protocol version mismatch prevents invitation consumption and Session use.
- Prompt rendering uses the invitation's stored template version. Template
  upgrade requires invitation rotation.
