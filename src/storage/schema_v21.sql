CREATE TABLE task_integration_integrity (
    task_id TEXT PRIMARY KEY,
    target_branch TEXT NOT NULL,
    target_head TEXT,
    integrated_set_digest TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('valid','drift','unobservable')),
    missing_change_sets_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(missing_change_sets_json)),
    details_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(details_json)),
    checked_at INTEGER NOT NULL,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

ALTER TABLE invalidations
    ADD COLUMN management_mode TEXT NOT NULL DEFAULT 'agent'
        CHECK (management_mode IN ('agent','system_reconciled'));

CREATE INDEX idx_integrity_status
    ON task_integration_integrity(status,checked_at);
