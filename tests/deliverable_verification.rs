use rusqlite::params;
use sessionbridge::{
    ErrorCode,
    app::{deliverable::DeliverableService, task::WorkspaceBinding},
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
    workspace: WorkspaceId,
    task: TaskId,
}
impl Harness {
    fn new() -> Self {
        let dir = tempdir().unwrap().keep();
        let db = dir.join("state.db");
        SqliteStore::migrate(&db).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        let workspace = WorkspaceId::new();
        let task = TaskId::new();
        store.connection().execute("INSERT INTO workspaces(id,name,repository_identity,created_at) VALUES(?1,'test','repo',0)",[workspace.to_string()]).unwrap();
        store.connection().execute("INSERT INTO tasks(id,workspace_id,title,phase,autonomy_policy,completion_policy,created_at,updated_at) VALUES(?1,?2,'test','active','open','all_workers_concluded',0,0)",params![task.to_string(),workspace.to_string()]).unwrap();
        Self {
            store,
            clock: FakeClock::new(OffsetDateTime::UNIX_EPOCH),
            workspace,
            task,
        }
    }
    fn binding(&self) -> WorkspaceBinding<'static> {
        WorkspaceBinding {
            id: self.workspace,
            repository_identity: "repo",
        }
    }
    fn deliverable(&self, kind: &str) -> String {
        let id = Uuid::now_v7().to_string();
        self.store.connection().execute("INSERT INTO deliverables(id,task_id,key,title,kind,required,source,status,created_at,updated_at) VALUES(?1,?2,?3,'deliverable',?4,1,'user','open',0,0)",params![id,self.task.to_string(),Uuid::now_v7().to_string(),kind]).unwrap();
        id
    }
    fn evidence(
        &self,
        subject_type: Option<&str>,
        subject_id: Option<&str>,
        commit: Option<&str>,
    ) -> String {
        let id = Uuid::now_v7().to_string();
        self.store.connection().execute("INSERT INTO evidence(id,task_id,kind,result,details_json,created_at,subject_type,subject_id,repository_identity,commit_hash,environment_json,observed_at) VALUES(?1,?2,'verification','passed','{}',0,?3,?4,'repo',?5,'{}',0)",params![id,self.task.to_string(),subject_type,subject_id,commit]).unwrap();
        id
    }
}

#[test]
fn non_code_deliverable_requires_evidence_bound_to_that_deliverable() {
    let mut h = Harness::new();
    let deliverable = h.deliverable("documentation");
    let generic = h.evidence(None, None, None);
    let binding = h.binding();
    let error = DeliverableService::new(h.clock.clone())
        .verify(
            h.store.connection_mut(),
            binding,
            h.task,
            &deliverable,
            &generic,
        )
        .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ItemNotReady);
    let bound = h.evidence(Some("deliverable"), Some(&deliverable), None);
    let binding = h.binding();
    let verified = DeliverableService::new(h.clock.clone())
        .verify(
            h.store.connection_mut(),
            binding,
            h.task,
            &deliverable,
            &bound,
        )
        .unwrap();
    assert_eq!(verified.status, "verified");
}

#[test]
fn code_deliverable_requires_accepted_work_integrated_commit_and_bound_evidence() {
    let mut h = Harness::new();
    let deliverable = h.deliverable("cli");
    let item = Uuid::now_v7().to_string();
    let session = Uuid::now_v7().to_string();
    let claim = Uuid::now_v7().to_string();
    let attempt = Uuid::now_v7().to_string();
    h.store.connection().execute("INSERT INTO agent_sessions(id,task_id,token_hash,label,protocol_version,last_seen_at,created_at,updated_at) VALUES(?1,?2,?3,'agent',1,0,0,0)",params![session,h.task.to_string(),vec![7_u8;32]]).unwrap();
    h.store.connection().execute("INSERT INTO work_items(id,task_id,kind,title,risk,plan_state,result_state,version,created_at,updated_at,accepted_at) VALUES(?1,?2,'implementation','code','low','active','accepted',1,0,0,0)",params![item,h.task.to_string()]).unwrap();
    h.store.connection().execute("INSERT INTO deliverable_coverage(deliverable_id,work_item_id,created_at) VALUES(?1,?2,0)",params![deliverable,item]).unwrap();
    h.store.connection().execute("INSERT INTO work_claims(id,task_id,work_item_id,session_id,mode,generation,claimed_at,lease_expires_at,closed_at,close_reason) VALUES(?1,?2,?3,?4,'execute',1,0,1,1,'submitted')",params![claim,h.task.to_string(),item,session]).unwrap();
    h.store.connection().execute("INSERT INTO work_attempts(id,task_id,claim_id,work_item_id,session_id,base_plan_version,status,started_at,ended_at) VALUES(?1,?2,?3,?4,?5,0,'submitted',0,1)",params![attempt,h.task.to_string(),claim,item,session]).unwrap();
    let change_set = Uuid::now_v7().to_string();
    h.store.connection().execute("INSERT INTO change_sets(id,task_id,attempt_id,base_commit,head_commit,changed_resources_json,integration_state,created_at,updated_at) VALUES(?1,?2,?3,'base','integrated-head','[]','integrated',0,0)",params![change_set,h.task.to_string(),attempt]).unwrap();
    let wrong = h.evidence(Some("work_item"), Some(&item), Some("other-head"));
    let binding = h.binding();
    assert_eq!(
        DeliverableService::new(h.clock.clone())
            .verify(
                h.store.connection_mut(),
                binding,
                h.task,
                &deliverable,
                &wrong
            )
            .unwrap_err()
            .code(),
        ErrorCode::ItemNotReady
    );
    let correct = h.evidence(Some("work_item"), Some(&item), Some("integrated-head"));
    let binding = h.binding();
    assert_eq!(
        DeliverableService::new(h.clock.clone())
            .verify(
                h.store.connection_mut(),
                binding,
                h.task,
                &deliverable,
                &correct
            )
            .unwrap()
            .status,
        "verified"
    );
    h.store
        .connection()
        .execute(
            "INSERT INTO task_integration_integrity(
            task_id,target_branch,target_head,integrated_set_digest,status,
            missing_change_sets_json,details_json,checked_at
         ) VALUES(?1,'main','other-head','digest','drift',?2,'{}',0)",
            params![
                h.task.to_string(),
                serde_json::json!([{"change_set_id":change_set}]).to_string()
            ],
        )
        .unwrap();
    let binding = h.binding();
    assert_eq!(
        DeliverableService::new(h.clock.clone())
            .verify(
                h.store.connection_mut(),
                binding,
                h.task,
                &deliverable,
                &correct
            )
            .unwrap_err()
            .code(),
        ErrorCode::ItemNotReady
    );
}
