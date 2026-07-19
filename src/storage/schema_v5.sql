CREATE TABLE integration_preparations (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    change_set_id TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    claim_generation INTEGER NOT NULL CHECK (claim_generation > 0),
    target_branch TEXT NOT NULL CHECK (length(trim(target_branch)) > 0),
    target_head_at_prepare TEXT NOT NULL CHECK (length(target_head_at_prepare) > 0),
    change_set_head TEXT NOT NULL CHECK (length(change_set_head) > 0),
    status TEXT NOT NULL DEFAULT 'prepared' CHECK (status IN ('prepared', 'confirmed', 'failed', 'expired')),
    request_id TEXT NOT NULL CHECK (length(request_id) > 0),
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    confirmed_at INTEGER,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (work_item_id, task_id) REFERENCES work_items(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (change_set_id, task_id) REFERENCES change_sets(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (claim_id) REFERENCES work_claims(id) ON DELETE RESTRICT,
    UNIQUE (session_id, request_id)
);

CREATE INDEX idx_integration_preparations_task_status
    ON integration_preparations(task_id, status, expires_at);
CREATE UNIQUE INDEX uq_integration_preparations_active_changeset
    ON integration_preparations(change_set_id) WHERE status = 'prepared';
