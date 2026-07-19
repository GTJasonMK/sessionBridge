CREATE TABLE await_delivery_outbox (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    wait_id TEXT NOT NULL UNIQUE,
    task_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered')),
    delivery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
    created_at INTEGER NOT NULL,
    delivered_at INTEGER,
    FOREIGN KEY (wait_id) REFERENCES agent_waits(id) ON DELETE CASCADE,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id, task_id) REFERENCES agent_sessions(id, task_id) ON DELETE RESTRICT
);

CREATE INDEX idx_await_delivery_outbox_status
    ON await_delivery_outbox(status, created_at);
