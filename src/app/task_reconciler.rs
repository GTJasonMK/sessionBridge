use std::{collections::BTreeSet, path::Path};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Result,
    app::{
        deliverable::evaluate_deliverable,
        events::{self, EventActor, EventEntity},
    },
    clock::{Clock, unix_micros},
    domain::{TaskId, WorkspaceId},
    error::ErrorCode,
    git::GitRepository,
    storage::SqliteStore,
    workspace::LocatedWorkspace,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissingIntegratedChangeSet {
    pub change_set_id: String,
    pub head_commit: String,
    pub work_item_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationIntegrityObservation {
    pub target_branch: String,
    pub target_head: Option<String>,
    pub integrated_set_digest: String,
    pub status: String,
    pub missing_change_sets: Vec<MissingIntegratedChangeSet>,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReconciliationResult {
    pub integrity_status: Option<String>,
    pub integrity_transitions: usize,
    pub requirement_transitions: usize,
    pub deliverable_transitions: usize,
    pub event_sequence: Option<i64>,
}

pub fn load_integrity(
    connection: &Connection,
    task_id: TaskId,
) -> Result<Option<IntegrationIntegrityObservation>> {
    connection
        .query_row(
            "SELECT target_branch,target_head,integrated_set_digest,status,
                    missing_change_sets_json,details_json
             FROM task_integration_integrity WHERE task_id=?1",
            [task_id.to_string()],
            |row| {
                let missing: String = row.get(4)?;
                let details: String = row.get(5)?;
                Ok(IntegrationIntegrityObservation {
                    target_branch: row.get(0)?,
                    target_head: row.get(1)?,
                    integrated_set_digest: row.get(2)?,
                    status: row.get(3)?,
                    missing_change_sets: serde_json::from_str(&missing)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    details: serde_json::from_str(&details)
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub struct TaskReconciler<C> {
    clock: C,
}

impl<C: Clock> TaskReconciler<C> {
    pub fn new(clock: C) -> Self {
        Self { clock }
    }

    pub fn reconcile_workspace(
        &self,
        store: &mut SqliteStore,
        workspace: &LocatedWorkspace,
        task_id: TaskId,
    ) -> Result<ReconciliationResult> {
        let observation = match workspace.repository.kind {
            crate::workspace::RepositoryKind::Git => Some(observe_integration(
                store.connection(),
                task_id,
                &workspace.paths.root,
            )?),
            crate::workspace::RepositoryKind::Directory => None,
        };
        let tx = store.begin_immediate()?;
        let result = reconcile_in_transaction(
            &tx,
            workspace.config.project_id,
            task_id,
            observation.as_ref(),
            &self.clock,
        )?;
        tx.commit()?;
        Ok(result)
    }

    pub fn reconcile_agent(
        &self,
        store: &mut SqliteStore,
        context: &crate::domain::SessionContext,
    ) -> Result<ReconciliationResult> {
        let (repository_path, git_base_commit): (Option<String>, Option<String>) =
            store.connection().query_row(
                "SELECT worktree_path,base_commit FROM agent_sessions WHERE id=?1 AND task_id=?2",
                params![context.session_id.to_string(), context.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        let observation = if git_base_commit
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            repository_path
                .as_deref()
                .map(Path::new)
                .map(|path| observe_integration(store.connection(), context.task_id, path))
                .transpose()?
        } else {
            None
        };
        let tx = store.begin_immediate()?;
        let result = reconcile_in_transaction(
            &tx,
            context.workspace_id,
            context.task_id,
            observation.as_ref(),
            &self.clock,
        )?;
        tx.commit()?;
        Ok(result)
    }
}

pub fn observe_integration(
    connection: &Connection,
    task_id: TaskId,
    repository_path: &Path,
) -> Result<IntegrationIntegrityObservation> {
    let configured_target: Option<String> = connection.query_row(
        "SELECT target_branch FROM tasks WHERE id=?1",
        [task_id.to_string()],
        |row| row.get(0),
    )?;
    let mut change_sets = connection
        .prepare(
            "SELECT c.id,c.head_commit,a.work_item_id
             FROM change_sets c JOIN work_attempts a ON a.id=c.attempt_id
             WHERE c.task_id=?1 AND c.integration_state='integrated'
             ORDER BY c.id",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    change_sets.sort();
    let mut hasher = Sha256::new();
    for (id, head, _) in &change_sets {
        hasher.update(id.as_bytes());
        hasher.update([0]);
        hasher.update(head.as_bytes());
        hasher.update([0xff]);
    }
    let digest = format!("{:x}", hasher.finalize());
    let repository = match GitRepository::open(repository_path) {
        Ok(repository) => repository,
        Err(error) => {
            return Ok(IntegrationIntegrityObservation {
                target_branch: configured_target.unwrap_or_default(),
                target_head: None,
                integrated_set_digest: digest,
                status: "unobservable".into(),
                missing_change_sets: Vec::new(),
                details: serde_json::json!({"error": error.to_string()}),
            });
        }
    };
    let target_branch = match configured_target {
        Some(branch) => branch,
        None => match repository.current_branch()? {
            Some(branch) => branch,
            None => {
                return Ok(IntegrationIntegrityObservation {
                    target_branch: String::new(),
                    target_head: None,
                    integrated_set_digest: digest,
                    status: "unobservable".into(),
                    missing_change_sets: Vec::new(),
                    details: serde_json::json!({"error":"Task has no target branch and repository is detached"}),
                });
            }
        },
    };
    let target_head = match repository.resolve_revision(&target_branch) {
        Ok(head) => head,
        Err(error) => {
            return Ok(IntegrationIntegrityObservation {
                target_branch,
                target_head: None,
                integrated_set_digest: digest,
                status: "unobservable".into(),
                missing_change_sets: Vec::new(),
                details: serde_json::json!({"error":error.to_string()}),
            });
        }
    };
    let mut missing = Vec::new();
    for (id, head, work_item_id) in change_sets {
        if let Err(error) = repository.require_ancestor(&head, &target_branch) {
            if error.code() == ErrorCode::TargetBranchNotIntegrated {
                missing.push(MissingIntegratedChangeSet {
                    change_set_id: id,
                    head_commit: head,
                    work_item_id,
                });
            } else {
                return Ok(IntegrationIntegrityObservation {
                    target_branch,
                    target_head: Some(target_head),
                    integrated_set_digest: digest,
                    status: "unobservable".into(),
                    missing_change_sets: Vec::new(),
                    details: serde_json::json!({"error":error.to_string()}),
                });
            }
        }
    }
    Ok(IntegrationIntegrityObservation {
        target_branch,
        target_head: Some(target_head),
        integrated_set_digest: digest,
        status: if missing.is_empty() { "valid" } else { "drift" }.into(),
        missing_change_sets: missing,
        details: serde_json::json!({}),
    })
}

pub fn reconcile_in_transaction<C: Clock>(
    transaction: &Transaction<'_>,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    observation: Option<&IntegrationIntegrityObservation>,
    clock: &C,
) -> Result<ReconciliationResult> {
    let now = clock.now();
    let mut result = ReconciliationResult::default();
    let mut changed = false;
    if let Some(observation) = observation {
        let (transitions, sequence) =
            reconcile_integrity(transaction, workspace_id, task_id, observation, now)?;
        result.integrity_status = Some(observation.status.clone());
        result.integrity_transitions = transitions;
        result.event_sequence = sequence;
        changed |= transitions > 0;
    }
    let missing = observation
        .map(|item| {
            item.missing_change_sets
                .iter()
                .map(|change_set| change_set.change_set_id.as_str())
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let (requirement_transitions, requirement_sequence) =
        reconcile_requirement_coverage(transaction, workspace_id, task_id, &missing, now)?;
    result.requirement_transitions = requirement_transitions;
    result.event_sequence = requirement_sequence.or(result.event_sequence);
    let (deliverable_transitions, deliverable_sequence) =
        reconcile_deliverables(transaction, workspace_id, task_id, now)?;
    result.deliverable_transitions = deliverable_transitions;
    result.event_sequence = deliverable_sequence.or(result.event_sequence);
    changed |= result.requirement_transitions > 0 || result.deliverable_transitions > 0;
    if changed {
        transaction.execute(
            "UPDATE tasks SET completion_revision=completion_revision+1,
                    version=version+1,updated_at=?2 WHERE id=?1",
            params![task_id.to_string(), unix_micros(now)],
        )?;
    }
    Ok(result)
}

fn reconcile_integrity(
    tx: &Transaction<'_>,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    observation: &IntegrationIntegrityObservation,
    now: time::OffsetDateTime,
) -> Result<(usize, Option<i64>)> {
    let previous: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT status,target_head,integrated_set_digest
             FROM task_integration_integrity WHERE task_id=?1",
            [task_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    tx.execute(
        "INSERT INTO task_integration_integrity(
            task_id,target_branch,target_head,integrated_set_digest,status,
            missing_change_sets_json,details_json,checked_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
         ON CONFLICT(task_id) DO UPDATE SET
            target_branch=excluded.target_branch,target_head=excluded.target_head,
            integrated_set_digest=excluded.integrated_set_digest,status=excluded.status,
            missing_change_sets_json=excluded.missing_change_sets_json,
            details_json=excluded.details_json,checked_at=excluded.checked_at",
        params![
            task_id.to_string(),
            observation.target_branch,
            observation.target_head,
            observation.integrated_set_digest,
            observation.status,
            serde_json::to_string(&observation.missing_change_sets)?,
            serde_json::to_string(&observation.details)?,
            unix_micros(now),
        ],
    )?;
    let missing_ids = observation
        .missing_change_sets
        .iter()
        .map(|item| item.change_set_id.as_str())
        .collect::<BTreeSet<_>>();
    let open = tx
        .prepare(
            "SELECT id,target_id FROM invalidations
             WHERE task_id=?1 AND source_type='target_branch_integrity'
               AND status='open' AND management_mode='system_reconciled'",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut transitions = 0;
    let mut last_sequence = None;
    for (id, target_id) in open {
        if !missing_ids.contains(target_id.as_str()) {
            tx.execute(
                "UPDATE invalidations SET status='resolved',resolved_at=?2 WHERE id=?1",
                params![id, unix_micros(now)],
            )?;
            transitions += 1;
            last_sequence = Some(events::append(
                tx,
                workspace_id,
                Some(&task_id.to_string()),
                "integration.integrity_restored",
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "change_set",
                    id: Some(&target_id),
                },
                &serde_json::json!({"change_set_id":target_id,"target_head":observation.target_head}),
                now,
            )?);
        }
    }
    for item in &observation.missing_change_sets {
        let existed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM invalidations WHERE task_id=?1
                AND source_type='target_branch_integrity' AND source_id=?2
                AND target_type='change_set' AND target_id=?3 AND status='open')",
            params![
                task_id.to_string(),
                observation.target_branch,
                item.change_set_id
            ],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO invalidations(
                id,task_id,source_type,source_id,target_type,target_id,
                old_revision,new_revision,status,reason,created_at,management_mode
             ) VALUES(?1,?2,'target_branch_integrity',?3,'change_set',?4,?5,?6,
                      'open','Integrated ChangeSet is no longer contained in the target branch',?7,
                      'system_reconciled')
             ON CONFLICT(task_id,source_type,source_id,target_type,target_id)
             WHERE status='open' DO UPDATE SET old_revision=excluded.old_revision,
                 new_revision=excluded.new_revision,reason=excluded.reason",
            params![
                uuid::Uuid::now_v7().to_string(),
                task_id.to_string(),
                observation.target_branch,
                item.change_set_id,
                item.head_commit,
                observation.target_head.as_deref().unwrap_or("unresolved"),
                unix_micros(now),
            ],
        )?;
        if !existed {
            transitions += 1;
            last_sequence = Some(events::append(
                tx,
                workspace_id,
                Some(&task_id.to_string()),
                "integration.integrity_drift_detected",
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "change_set",
                    id: Some(&item.change_set_id),
                },
                &serde_json::json!({
                    "change_set_id":item.change_set_id,"work_item_id":item.work_item_id,
                    "head_commit":item.head_commit,"target_branch":observation.target_branch,
                    "target_head":observation.target_head,
                }),
                now,
            )?);
        }
    }
    let has_integrated_change_sets: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM change_sets
         WHERE task_id=?1 AND integration_state='integrated')",
        [task_id.to_string()],
        |row| row.get(0),
    )?;
    let observation_changed = previous.as_ref().map_or(
        has_integrated_change_sets || observation.status != "valid",
        |(status, head, digest)| {
            status != &observation.status
                || head != &observation.target_head
                || digest != &observation.integrated_set_digest
        },
    );
    if observation_changed {
        if transitions == 0 {
            transitions = 1;
        }
        last_sequence = Some(events::append(
            tx,
            workspace_id,
            Some(&task_id.to_string()),
            "integration.integrity_observed",
            EventActor {
                kind: "system",
                id: None,
            },
            EventEntity {
                kind: "task",
                id: Some(&task_id.to_string()),
            },
            &serde_json::json!({
                "previous_status": previous.as_ref().map(|value| value.0.as_str()),
                "status": observation.status,
                "target_branch": observation.target_branch,
                "target_head": observation.target_head,
                "integrated_set_digest": observation.integrated_set_digest,
                "missing_change_set_count": observation.missing_change_sets.len(),
            }),
            now,
        )?);
    }
    Ok((transitions, last_sequence))
}

fn reconcile_requirement_coverage(
    tx: &Transaction<'_>,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    missing: &BTreeSet<&str>,
    now: time::OffsetDateTime,
) -> Result<(usize, Option<i64>)> {
    let rows = tx
        .prepare(
            "SELECT rc.id,rc.covered_by_id,rc.coverage_state,w.result_state,
                    EXISTS(SELECT 1 FROM evidence e
                           WHERE e.task_id=rc.task_id AND e.result='passed'
                             AND (e.subject_type='work_item' AND e.subject_id=w.id
                                  OR e.attempt_id IN (SELECT id FROM work_attempts WHERE work_item_id=w.id))),
                    (SELECT json_group_array(c.id) FROM change_sets c
                     JOIN work_attempts a ON a.id=c.attempt_id
                     WHERE a.work_item_id=w.id AND c.integration_state='integrated')
             FROM requirement_coverage rc JOIN work_items w ON w.id=rc.covered_by_id
             WHERE rc.task_id=?1 AND rc.covered_by_type='work_item'",
        )?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?, row.get::<_, String>(5)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut changed = 0;
    let mut last_sequence = None;
    for (id, work_item_id, current, result_state, has_evidence, integrated_json) in rows {
        let integrated_ids: Vec<String> = serde_json::from_str(&integrated_json)?;
        let valid_integrated = integrated_ids
            .iter()
            .any(|value| !missing.contains(value.as_str()));
        let next = if result_state == "accepted" && has_evidence && valid_integrated {
            "integrated"
        } else if has_evidence {
            "verified"
        } else if result_state == "submitted" || result_state == "accepted" {
            "implemented"
        } else {
            "planned"
        };
        if current != next {
            tx.execute(
                "UPDATE requirement_coverage SET coverage_state=?2,updated_at=?3 WHERE id=?1",
                params![id, next, unix_micros(now)],
            )?;
            last_sequence = Some(events::append(
                tx,
                workspace_id,
                Some(&task_id.to_string()),
                "requirement.coverage_reconciled",
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "requirement_coverage",
                    id: Some(&id),
                },
                &serde_json::json!({
                    "work_item_id": work_item_id,
                    "previous_state": current,
                    "state": next,
                    "source": "task_reconciler",
                }),
                now,
            )?);
            changed += 1;
        }
    }
    Ok((changed, last_sequence))
}

fn reconcile_deliverables(
    tx: &Transaction<'_>,
    workspace_id: WorkspaceId,
    task_id: TaskId,
    now: time::OffsetDateTime,
) -> Result<(usize, Option<i64>)> {
    let deliverables = tx
        .prepare("SELECT id,status,kind FROM deliverables WHERE task_id=?1 AND status<>'waived'")?
        .query_map([task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut changed = 0;
    let mut last_sequence = None;
    for (id, current, kind) in deliverables {
        let evaluation = evaluate_deliverable(tx, task_id, &id, &kind, None)?;
        if let Some(evidence_id) = evaluation.evidence_id {
            tx.execute(
                "INSERT OR IGNORE INTO deliverable_evidence(deliverable_id,evidence_id,created_at)
                 VALUES(?1,?2,?3)",
                params![id, evidence_id, unix_micros(now)],
            )?;
        }
        if current != evaluation.status {
            tx.execute(
                "UPDATE deliverables SET status=?2,version=version+1,updated_at=?3 WHERE id=?1",
                params![id, evaluation.status, unix_micros(now)],
            )?;
            let event_type = if evaluation.status == "verified" {
                "deliverable.verified"
            } else {
                "deliverable.verification_invalidated"
            };
            last_sequence = Some(events::append(
                tx,
                workspace_id,
                Some(&task_id.to_string()),
                event_type,
                EventActor {
                    kind: "system",
                    id: None,
                },
                EventEntity {
                    kind: "deliverable",
                    id: Some(&id),
                },
                &serde_json::json!({
                    "previous_status":current,
                    "status":evaluation.status,
                    "source":"task_reconciler"
                }),
                now,
            )?);
            changed += 1;
        }
    }
    Ok((changed, last_sequence))
}
