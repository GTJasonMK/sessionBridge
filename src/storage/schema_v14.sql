ALTER TABLE proposal_consolidation_requests ADD COLUMN claimed_by_session_id TEXT;
ALTER TABLE proposal_consolidation_requests ADD COLUMN resolution_json TEXT
    CHECK (resolution_json IS NULL OR json_valid(resolution_json));

CREATE INDEX idx_proposal_consolidation_owner
    ON proposal_consolidation_requests(claimed_by_session_id,status);
