//! COPY reports command-local counters, not a native phase or completion of the request.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PostgresCopyCommand {
    From,
    To,
}

impl PostgresCopyCommand {
    pub fn from_native(value: &str) -> Option<Self> {
        match value {
            "COPY FROM" => Some(Self::From),
            "COPY TO" => Some(Self::To),
            _ => None,
        }
    }

    pub fn as_native(self) -> &'static str {
        match self {
            Self::From => "COPY FROM",
            Self::To => "COPY TO",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum PostgresCopyIo {
    File,
    Program,
    Pipe,
    Callback,
}

impl PostgresCopyIo {
    pub fn from_native(value: &str) -> Option<Self> {
        match value {
            "FILE" => Some(Self::File),
            "PROGRAM" => Some(Self::Program),
            "PIPE" => Some(Self::Pipe),
            "CALLBACK" => Some(Self::Callback),
            _ => None,
        }
    }

    pub fn as_native(self) -> &'static str {
        match self {
            Self::File => "FILE",
            Self::Program => "PROGRAM",
            Self::Pipe => "PIPE",
            Self::Callback => "CALLBACK",
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct PostgresCopyProgress {
    pub database_oid: String,
    /// Native zero denotes COPY from a SELECT query, not an unknown table identity.
    pub relation_oid: String,
    pub command: PostgresCopyCommand,
    pub io_type: PostgresCopyIo,
    pub bytes_processed: Option<String>,
    /// Native zero means unavailable and never establishes a completion denominator.
    pub bytes_total: Option<String>,
    pub tuples_processed: Option<String>,
    pub tuples_excluded: Option<String>,
    /// Absent on versions without the native counter; zero is retained only when reported.
    pub tuples_skipped: Option<String>,
}

impl PostgresCopyProgress {
    pub fn is_valid(&self) -> bool {
        fn oid(value: &str, nonzero: bool) -> bool {
            value
                .parse::<u32>()
                .is_ok_and(|value_number| (!nonzero || value_number != 0) && value_number.to_string() == value)
        }
        oid(&self.database_oid, true)
            && oid(&self.relation_oid, false)
            && (self.relation_oid != "0" || self.command == PostgresCopyCommand::To)
            && (self.command != PostgresCopyCommand::To || self.bytes_total.as_deref().is_none_or(|value| value == "0"))
            && [
                &self.bytes_processed,
                &self.bytes_total,
                &self.tuples_processed,
                &self.tuples_excluded,
                &self.tuples_skipped,
            ]
            .into_iter()
            .flatten()
            .all(|value| value.parse::<i64>().is_ok_and(|number| number >= 0 && number.to_string() == *value))
    }
}
