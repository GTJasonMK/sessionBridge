use rusqlite::params;
use sessionbridge::{
    app::task_reconciler::{
        IntegrationIntegrityObservation, MissingIntegratedChangeSet, reconcile_in_transaction,
    },
    clock::FakeClock,
    domain::{TaskId, WorkspaceId},
    storage::SqliteStore,
};
use tempfile::tempdir;
use time::OffsetDateTime;
use uuid::Uuid;

struct Harness {
    store: SqliteStore,
    clock: FakeClock,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    requirement_coverage_id: String,
    deliverable_id: String,
    change_set_id: String,
}

impl Harness {
    fn new() -> Self {
        let directory = tempdir().unwrap().keep();
        let database = directory.join("state.db");
        SqliteStore::migrate(&database).unwrap();
        let store = SqliteStore::open(&database).unwrap();
        let workspace_id = WorkspaceId::new();
        let task_id = TaskId::new();
        let session_id = Uuid::now_v7().to_string();
        let work_item_id = Uuid::now_v7().to_string();
        let claim_id = Uuid::now_v7().to_string();
        let attempt_id = Uuid::now_v7().to_string();
        let change_set_id = Uuid::now_v7().to_string();
        let evidence_id = Uuid::now_v7().to_string();
        let requirement_id = Uuid::now_v7().to_string();
        let requirement_coverage_id = Uuid::now_v7().to_string();
        let deliverable_id = Uuid::now_v7().to_string();

        store
            .connection()
            .execute(
                "INSERT INTO workspaces(id,name,repository_identity,created_at)
                 VALUES(?1,'test','repo',0)",
                [workspace_id.to_string()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO tasks(
                    id,workspace_id,title,phase,autonomy_policy,completion_policy,
                    target_branch,created_at,updated_at
                 ) VALUES(?1,?2,'test','active','open','all_workers_concluded','main',0,0)",
                params![task_id.to_string(), workspace_id.to_string()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO agent_sessions(
                    id,task_id,token_hash,label,protocol_version,last_seen_at,created_at,updated_at
                 ) VALUES(?1,?2,?3,'agent',1,0,0,0)",
                params![session_id, task_id.to_string(), vec![7_u8; 32]],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO work_items(
                    id,task_id,kind,title,risk,plan_state,result_state,version,
                    created_at,updated_at,accepted_at
                 ) VALUES(?1,?2,'implementation','code','low','active','accepted',1,0,0,0)",
                params![work_item_id, task_id.to_string()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO work_claims(
                    id,task_id,work_item_id,session_id,mode,generation,claimed_at,
                    lease_expires_at,closed_at,close_reason
                 ) VALUES(?1,?2,?3,?4,'execute',1,0,1,1,'submitted')",
                params![claim_id, task_id.to_string(), work_item_id, session_id],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO work_attempts(
                    id,task_id,claim_id,work_item_id,session_id,base_plan_version,
                    base_commit,status,started_at,ended_at
                 ) VALUES(?1,?2,?3,?4,?5,0,'base','submitted',0,1)",
                params![
                    attempt_id,
                    task_id.to_string(),
                    claim_id,
                    work_item_id,
                    session_id
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO change_sets(
                    id,task_id,attempt_id,base_commit,head_commit,changed_resources_json,
                    integration_state,created_at,updated_at
                 ) VALUES(?1,?2,?3,'base','integrated-head','[]','integrated',0,0)",
                params![change_set_id, task_id.to_string(), attempt_id],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO evidence(
                    id,task_id,attempt_id,kind,result,details_json,created_by_session_id,
                    created_at,subject_type,subject_id,repository_identity,commit_hash,
                    environment_json,observed_at
                 ) VALUES(?1,?2,?3,'verification','passed','{}',?4,0,
                          'work_item',?5,'repo','integrated-head','{}',0)",
                params![
                    evidence_id,
                    task_id.to_string(),
                    attempt_id,
                    session_id,
                    work_item_id
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO requirements(
                    id,task_id,kind,title,required,status,source,created_at,updated_at
                 ) VALUES(?1,?2,'functional','requirement',1,'active','user',0,0)",
                params![requirement_id, task_id.to_string()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO requirement_coverage(
                    id,task_id,requirement_id,covered_by_type,covered_by_id,
                    coverage_state,created_at,updated_at
                 ) VALUES(?1,?2,?3,'work_item',?4,'integrated',0,0)",
                params![
                    requirement_coverage_id,
                    task_id.to_string(),
                    requirement_id,
                    work_item_id
                ],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO deliverables(
                    id,task_id,key,title,kind,required,source,status,created_at,updated_at
                 ) VALUES(?1,?2,'cli','CLI','cli',1,'user','verified',0,0)",
                params![deliverable_id, task_id.to_string()],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO deliverable_coverage(deliverable_id,work_item_id,created_at)
                 VALUES(?1,?2,0)",
                params![deliverable_id, work_item_id],
            )
            .unwrap();
        store
            .connection()
            .execute(
                "INSERT INTO deliverable_evidence(deliverable_id,evidence_id,created_at)
                 VALUES(?1,?2,0)",
                params![deliverable_id, evidence_id],
            )
            .unwrap();

        Self {
            store,
            clock: FakeClock::new(OffsetDateTime::UNIX_EPOCH),
            workspace_id,
            task_id,
            requirement_coverage_id,
            deliverable_id,
            change_set_id,
        }
    }

    fn reconcile(&mut self, observation: &IntegrationIntegrityObservation) -> usize {
        let before: i64 = self
            .store
            .connection()
            .query_row(
                "SELECT completion_revision FROM tasks WHERE id=?1",
                [self.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        let transaction = self.store.begin_immediate().unwrap();
        reconcile_in_transaction(
            &transaction,
            self.workspace_id,
            self.task_id,
            Some(observation),
            &self.clock,
        )
        .unwrap();
        transaction.commit().unwrap();
        let after: i64 = self
            .store
            .connection()
            .query_row(
                "SELECT completion_revision FROM tasks WHERE id=?1",
                [self.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        usize::try_from(after - before).unwrap()
    }

    fn observation(&self, status: &str) -> IntegrationIntegrityObservation {
        IntegrationIntegrityObservation {
            target_branch: "main".into(),
            target_head: Some(
                if status == "drift" {
                    "rewritten-head"
                } else {
                    "integrated-head"
                }
                .into(),
            ),
            integrated_set_digest: "digest".into(),
            status: status.into(),
            missing_change_sets: if status == "drift" {
                vec![MissingIntegratedChangeSet {
                    change_set_id: self.change_set_id.clone(),
                    head_commit: "integrated-head".into(),
                    work_item_id: self.work_item_id(),
                }]
            } else {
                Vec::new()
            },
            details: serde_json::json!({}),
        }
    }

    fn work_item_id(&self) -> String {
        self.store
            .connection()
            .query_row(
                "SELECT covered_by_id FROM requirement_coverage WHERE id=?1",
                [&self.requirement_coverage_id],
                |row| row.get(0),
            )
            .unwrap()
    }
}

#[test]
fn first_integrity_observation_changes_completion_revision_and_is_idempotent() {
    let mut harness = Harness::new();
    let valid = harness.observation("valid");

    assert_eq!(harness.reconcile(&valid), 1);
    assert_eq!(harness.reconcile(&valid), 0);

    let observed_events: i64 = harness
        .store
        .connection()
        .query_row(
            "SELECT count(*) FROM events
             WHERE task_id=?1 AND event_type='integration.integrity_observed'",
            [harness.task_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(observed_events, 1);
}

#[test]
fn drift_downgrades_derived_state_and_restoration_recovers_it() {
    let mut harness = Harness::new();
    let valid = harness.observation("valid");
    harness.reconcile(&valid);
    let drift = harness.observation("drift");

    assert_eq!(harness.reconcile(&drift), 1);
    let coverage: String = harness
        .store
        .connection()
        .query_row(
            "SELECT coverage_state FROM requirement_coverage WHERE id=?1",
            [&harness.requirement_coverage_id],
            |row| row.get(0),
        )
        .unwrap();
    let deliverable: String = harness
        .store
        .connection()
        .query_row(
            "SELECT status FROM deliverables WHERE id=?1",
            [&harness.deliverable_id],
            |row| row.get(0),
        )
        .unwrap();
    let invalidation: (String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT status,management_mode FROM invalidations
             WHERE target_id=?1 AND source_type='target_branch_integrity'",
            [&harness.change_set_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(coverage, "verified");
    assert_eq!(deliverable, "planned");
    assert_eq!(invalidation, ("open".into(), "system_reconciled".into()));
    assert_eq!(harness.reconcile(&drift), 0);

    assert_eq!(harness.reconcile(&valid), 1);
    let restored: (String, String, String) = harness
        .store
        .connection()
        .query_row(
            "SELECT
                (SELECT coverage_state FROM requirement_coverage WHERE id=?1),
                (SELECT status FROM deliverables WHERE id=?2),
                (SELECT status FROM invalidations
                 WHERE target_id=?3 AND source_type='target_branch_integrity')",
            params![
                harness.requirement_coverage_id,
                harness.deliverable_id,
                harness.change_set_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        restored,
        ("integrated".into(), "verified".into(), "resolved".into())
    );
}
