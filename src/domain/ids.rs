use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}

id_type!(WorkspaceId);
id_type!(TaskId);
id_type!(CriterionId);
id_type!(RequirementId);
id_type!(GapId);
id_type!(WorkItemId);
id_type!(InvitationId);
id_type!(SessionId);
id_type!(ParticipantId);
id_type!(ConclusionId);
id_type!(RetirementId);
id_type!(ClaimId);
id_type!(AttemptId);
id_type!(ArtifactId);
id_type!(EvidenceId);
id_type!(ChangeSetId);
id_type!(ContractId);
id_type!(DecisionId);
id_type!(CompletionId);
