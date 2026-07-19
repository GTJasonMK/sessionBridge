CREATE TABLE agent_action_proposals (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
    summary TEXT NOT NULL CHECK (length(trim(summary)) > 0),
    rationale TEXT NOT NULL DEFAULT '',
    payload_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload_json)),
    status TEXT NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed', 'accepted', 'rejected', 'superseded')),
    dedupe_key TEXT,
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT,
    UNIQUE (task_id, session_id, dedupe_key)
);

CREATE INDEX idx_agent_action_proposals_task_status
    ON agent_action_proposals(task_id, status, created_at);
