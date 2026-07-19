CREATE TABLE stage_handoffs (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    work_item_id TEXT NOT NULL,
    from_stage TEXT NOT NULL CHECK (from_stage IN ('execute','review')),
    to_stage TEXT NOT NULL CHECK (to_stage IN ('review','integrate')),
    coordinator_session_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'reserved' CHECK (status IN ('reserved','consumed','expired','cancelled')),
    expires_at INTEGER NOT NULL,
    consumed_by_claim_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (work_item_id, task_id) REFERENCES work_items(id, task_id) ON DELETE CASCADE,
    FOREIGN KEY (coordinator_session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT,
    FOREIGN KEY (consumed_by_claim_id, task_id) REFERENCES work_claims(id, task_id) ON DELETE RESTRICT
);

CREATE UNIQUE INDEX idx_stage_handoffs_reserved
    ON stage_handoffs(work_item_id, to_stage) WHERE status='reserved';

CREATE INDEX idx_stage_handoffs_expiry
    ON stage_handoffs(task_id, status, expires_at);
