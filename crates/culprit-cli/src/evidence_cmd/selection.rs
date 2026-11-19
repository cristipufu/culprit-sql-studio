//! Explicit companion selections share one action budget; their native and archive scopes stay independent.
use super::*;

pub(super) struct EvidenceSelection {
    pub common: CommonArgs,
    pub plan: ReadPlan,
}

pub(super) struct ReadPlan {
    pub families: Vec<EvidenceFamily>,
    pub analysis: Option<Analysis>,
    pub population: Option<NativeIdentity>,
    pub companions: CompanionReadPlan,
}

#[derive(Default)]
pub(super) struct CompanionReadPlan {
    pub activity: Option<String>,
    pub compilation_baseline: Option<String>,
    pub cache_baseline: Option<String>,
    pub blocking: Option<String>,
    pub definitions: Option<DefinitionRead>,
}

pub(super) struct DefinitionRead {
    pub range: String,
    pub kind: DefinitionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DefinitionKind {
    QueryStoreCompilation,
    QueryStoreRuntime,
    CachedStatement,
}

impl DefinitionKind {
    pub fn for_analysis(analysis: Analysis) -> Option<Self> {
        match analysis {
            Analysis::Compilation => Some(Self::QueryStoreCompilation),
            Analysis::PageServerReads => Some(Self::QueryStoreRuntime),
            Analysis::CompletedSpills => Some(Self::CachedStatement),
            _ => None,
        }
    }

    pub fn family(self) -> EvidenceFamily {
        match self {
            Self::QueryStoreCompilation | Self::QueryStoreRuntime => EvidenceFamily::QueryDefinitions,
            Self::CachedStatement => EvidenceFamily::ObjectDefinitions,
        }
    }

    pub fn accepts_primary(self, source: &culprit_core::telemetry::CollectionDescription) -> bool {
        if source.target.engine() != culprit_core::Engine::Mssql || source.target.database().is_none() {
            return false;
        }
        match self {
            Self::QueryStoreRuntime => {
                source.partition == culprit_core::telemetry::SourcePartition::Target
                    && source.query_history().is_some_and(|history| history.is_valid())
            }
            Self::QueryStoreCompilation | Self::CachedStatement => {
                let dataset = if self == Self::QueryStoreCompilation {
                    culprit_core::telemetry::SnapshotDataset::QueryCompilation
                } else {
                    culprit_core::telemetry::SnapshotDataset::CachedRuntime
                };
                source.observation.snapshot().is_some_and(|snapshot| snapshot.dataset == dataset)
            }
        }
    }
}

impl EvidenceSelection {
    pub fn from_command(command: EvidenceCommand) -> Result<Self> {
        Ok(match command {
            EvidenceCommand::Read(args) => Self {
                common: args.common,
                plan: ReadPlan {
                    families: args.families.into_iter().map(Into::into).collect(),
                    analysis: None,
                    population: None,
                    companions: CompanionReadPlan::default(),
                },
            },
            EvidenceCommand::Analyze(args) => {
                args.validate_selection()?;
                let definitions = match args.definitions_range {
                    Some(range) => Some(DefinitionRead {
                        range,
                        kind: DefinitionKind::for_analysis(args.analysis)
                            .ok_or_else(|| DeliveryProblem::InvalidContract.error())?,
                    }),
                    None => None,
                };
                Self {
                    common: args.common,
                    plan: ReadPlan {
                        families: args.analysis.families(),
                        analysis: Some(args.analysis),
                        population: args.workload_population,
                        companions: CompanionReadPlan {
                            activity: args.activity_range,
                            compilation_baseline: args.compilation_baseline,
                            cache_baseline: args.cache_baseline,
                            blocking: args.blocking_range,
                            definitions,
                        },
                    },
                }
            }
        })
    }
}
