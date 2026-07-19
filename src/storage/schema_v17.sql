ALTER TABLE work_items ADD COLUMN review_policy TEXT NOT NULL DEFAULT 'risk_based_review'
    CHECK (review_policy IN (
        'self_review_allowed',
        'independent_review_required',
        'capability_review_required',
        'risk_based_review'
    ));
