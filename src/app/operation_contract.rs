use serde::Serialize;

use crate::{
    Result,
    error::{ErrorCode, SessionBridgeError},
};

#[derive(Debug, Clone, Serialize)]
pub struct OperationContract {
    pub operation: &'static str,
    pub command: &'static str,
    pub purpose: &'static str,
    pub input_mode: &'static str,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
    pub invariants: &'static [&'static str],
    pub schema: serde_json::Value,
    pub template: serde_json::Value,
    pub example: &'static str,
}

pub fn list() -> Vec<OperationContract> {
    macro_rules! contract {
        ($operation:expr, $command:expr, $purpose:expr, $input_mode:expr,
         $required:expr, $optional:expr, $invariants:expr, $schema:expr,
         $template:expr, $example:expr $(,)?) => {
            OperationContract {
                operation: $operation,
                command: $command,
                purpose: $purpose,
                input_mode: $input_mode,
                required: $required,
                optional: $optional,
                invariants: $invariants,
                schema: $schema,
                template: $template,
                example: $example,
            }
        };
    }
    vec![
        contract!(
            "agent.next",
            "sessionbridge agent next --format json",
            "Read compact state, obligations, known legal actions, scoped tokens, chat, and operation references.",
            "typed_flags",
            &[],
            &["--limit", "--preview"],
            &["--preview never updates the Session heartbeat"],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge agent next --preview --format json",
        ),
        contract!(
            "work.start",
            "sessionbridge work start <WORK_ITEM>",
            "Acquire an execute Claim and create a WorkAttempt from the current target branch head.",
            "typed_flags",
            &["WORK_ITEM"],
            &["--resources-file", "--inputs-file", "--lease", "--dry-run"],
            &[
                "the registered worktree must be clean",
                "pre-existing commits require work.adopt",
                "write resources must not conflict with enforced Reservations",
            ],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge work start 019... --lease 30m --format json",
        ),
        contract!(
            "work.adopt",
            "sessionbridge work adopt <WORK_ITEM>",
            "Explicitly adopt clean commits already ahead of the Task target branch into a new WorkAttempt.",
            "typed_flags",
            &["WORK_ITEM"],
            &["--resources-file", "--inputs-file", "--lease", "--dry-run"],
            &[
                "the Task target head must be an ancestor of the worktree head",
                "diverged worktrees must be rebased before adoption",
            ],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge work adopt 019... --format json",
        ),
        contract!(
            "work.submit",
            "sessionbridge work submit <WORK_ITEM> --bundle <FILE>",
            "Atomically record Evidence, ChangeSet, Attempt completion, Claim closure, and stage handoff.",
            "json_file",
            &["WORK_ITEM", "--bundle"],
            &["--dry-run"],
            &[
                "ChangeSet base_commit must equal the WorkAttempt base",
                "changed resources are derived from Git for Git-backed Sessions",
            ],
            serde_json::json!({
                "type":"object",
                "required":["change_set"],
                "properties":{
                    "evidence":{"type":"array","items":{"$ref":"EvidenceInput"}},
                    "change_set":{
                        "type":"object",
                        "required":["base_commit","head_commit","changed_resources"],
                        "properties":{
                            "base_commit":{"type":"string"},
                            "head_commit":{"type":"string"},
                            "changed_resources":{"type":"array","items":{"type":"string"}},
                            "tests":{"type":"array"}
                        }
                    }
                }
            }),
            serde_json::json!({
                "evidence":[],
                "change_set":{
                    "base_commit":"<ATTEMPT_BASE_COMMIT>",
                    "head_commit":"<WORKTREE_HEAD_COMMIT>",
                    "changed_resources":[],
                    "tests":[]
                }
            }),
            "sessionbridge work submit 019... --bundle submit.json --format json",
        ),
        contract!(
            "work.review",
            "sessionbridge work review <WORK_ITEM> --decision <approved|changes-requested>",
            "Claim and complete a review using the WorkItem review policy.",
            "typed_flags",
            &["WORK_ITEM", "--decision"],
            &["--evidence-file", "--lease", "--dry-run"],
            &["independent review policy is enforced by executor identity"],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge work review 019... --decision approved --format json",
        ),
        contract!(
            "work.integrate",
            "sessionbridge work integrate <prepare|confirm>",
            "Prepare an integration lease, perform Git work outside SQLite, then confirm verified ancestry.",
            "typed_flags",
            &["prepare: WORK_ITEM", "confirm: PREPARATION"],
            &["--lease", "--details-file", "--dry-run"],
            &["confirm requires the target branch to contain the ChangeSet head"],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge work integrate prepare 019... --format json",
        ),
        contract!(
            "agent.await",
            "sessionbridge agent await work-items --item <WORK_ITEM>...",
            "Enter a durable wait only for dependencies with a live ownership chain.",
            "typed_flags",
            &["--item"],
            &["--mode"],
            &["active Claims or unsubmitted Attempts must be cleared before waiting"],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge agent await work-items --item 019... --mode all --format json",
        ),
        contract!(
            "claim.scope.reconcile",
            "sessionbridge claim scope reconcile <CLAIM> --fence <GENERATION> --from-git",
            "Batch-add uncovered writable resources from committed and dirty Git paths.",
            "typed_flags",
            &["CLAIM", "--fence", "--from-git"],
            &[],
            &[
                "only execute Claims can reconcile from Git",
                "the operation only adds missing coverage and never silently removes scope",
                "all additions are conflict-checked in one transaction",
            ],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge claim scope reconcile 019... --fence 1 --from-git --format json",
        ),
        contract!(
            "agent.conclude",
            "sessionbridge agent conclude --outcome <OUTCOME>",
            "Submit a semantic completion assessment using an automatically acquired completion scope.",
            "typed_flags",
            &["--outcome", "--summary and --checked, or --assessment-file"],
            &[
                "--risk",
                "--blocker",
                "--gap",
                "--coord-request",
                "--completion-token",
            ],
            &[
                "blocked conclusions require an entity-referenced open signal",
                "repeated terminal conclude returns the existing conclusion",
            ],
            serde_json::json!({}),
            serde_json::json!({}),
            "sessionbridge agent conclude --outcome no-further-work --summary 'No further work remains' --checked 'requirements, repository, tests, review and integration' --format json",
        ),
        contract!(
            "agent.propose-action",
            "sessionbridge agent propose-action --kind <KIND> --input <FILE>",
            "Record a novel Agent-proposed action without automatically executing unknown side effects.",
            "json_file",
            &["--kind", "--input"],
            &[],
            &["unknown action kinds are preserved and audited, never auto-executed"],
            serde_json::json!({
                "type":"object",
                "required":["summary"],
                "properties":{
                    "summary":{"type":"string"},
                    "rationale":{"type":"string"},
                    "payload":{"type":"object"},
                    "dedupe_key":{"type":["string","null"]},
                    "priority":{"enum":["low","normal","high","critical"]},
                    "safety_relevant":{"type":"boolean"},
                    "source_entities":{"type":"array","items":{"type":"string"}},
                    "provenance":{"type":"object"}
                }
            }),
            serde_json::json!({
                "summary":"<WHAT SHOULD HAPPEN>",
                "rationale":"<WHY IT HELPS THE TASK>",
                "payload":{},
                "priority":"normal",
                "safety_relevant":false,
                "source_entities":[],
                "provenance":{}
            }),
            "sessionbridge agent propose-action --kind investigation --input proposal.json --format json",
        ),
    ]
}

pub fn get(operation: &str) -> Result<OperationContract> {
    let normalized = operation.trim();
    list()
        .into_iter()
        .find(|contract| contract.operation == normalized)
        .ok_or_else(|| {
            SessionBridgeError::domain(
                ErrorCode::NotFound,
                format!("operation contract '{normalized}' was not found"),
            )
            .with_details(serde_json::json!({
                "available_operations": list()
                    .into_iter()
                    .map(|contract| contract.operation)
                    .collect::<Vec<_>>()
            }))
        })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::Cli;

    #[test]
    fn operation_contracts_are_unique_and_json_inputs_have_templates() {
        let contracts = list();
        let mut operations = std::collections::BTreeSet::new();
        for contract in contracts {
            assert!(operations.insert(contract.operation));
            assert!(!contract.command.is_empty());
            assert!(!contract.example.is_empty());
            if contract.input_mode == "json_file" {
                assert_eq!(contract.schema["type"], "object");
                assert!(contract.template.is_object());
            }
        }
    }

    #[test]
    fn operation_contract_examples_match_the_cli() {
        for contract in list() {
            let arguments = shlex::split(contract.example)
                .unwrap_or_else(|| panic!("invalid shell quoting in {}", contract.operation));
            Cli::try_parse_from(arguments).unwrap_or_else(|error| {
                panic!(
                    "operation contract {} has an invalid example: {error}",
                    contract.operation
                )
            });
        }
    }
}
