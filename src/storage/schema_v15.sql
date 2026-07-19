ALTER TABLE speculative_work ADD COLUMN adoption_proposal_id TEXT;
CREATE INDEX idx_speculative_adoption_proposal ON speculative_work(adoption_proposal_id);
