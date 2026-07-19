ALTER TABLE agent_action_proposals ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal'
    CHECK (priority IN ('low','normal','high','critical'));
ALTER TABLE agent_action_proposals ADD COLUMN safety_relevant INTEGER NOT NULL DEFAULT 0
    CHECK (safety_relevant IN (0,1));
ALTER TABLE agent_action_proposals ADD COLUMN semantic_fingerprint TEXT;
ALTER TABLE agent_action_proposals ADD COLUMN provenance_json TEXT NOT NULL DEFAULT '{}'
    CHECK (json_valid(provenance_json));
ALTER TABLE agent_action_proposals ADD COLUMN source_entities_json TEXT NOT NULL DEFAULT '[]'
    CHECK (json_valid(source_entities_json));
ALTER TABLE agent_action_proposals ADD COLUMN observed_cursor INTEGER NOT NULL DEFAULT 0
    CHECK (observed_cursor >= 0);

CREATE INDEX idx_agent_action_proposals_semantic
    ON agent_action_proposals(task_id,semantic_fingerprint,status);

CREATE TABLE proposal_consolidation_requests (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    task_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','claimed','resolved','cancelled')),
    proposal_count INTEGER NOT NULL CHECK (proposal_count > 0),
    threshold INTEGER NOT NULL CHECK (threshold > 0),
    reason TEXT NOT NULL CHECK (length(trim(reason)) > 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    resolved_at INTEGER,
    FOREIGN KEY (task_id) REFERENCES tasks(id) ON DELETE CASCADE
);

CREATE UNIQUE INDEX idx_proposal_consolidation_open
    ON proposal_consolidation_requests(task_id) WHERE status IN ('open','claimed');
