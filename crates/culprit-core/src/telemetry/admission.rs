//! Inspectable local source ownership. A route association is never native database identity.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum ConnectionTestKind {
    Settings,
    Ssh,
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceAdmissionRoute {
    SavedConnection { name: String },
    UnsavedConnectionTest { invocation_id: String },
    SavedConnectionTest { name: String, test: ConnectionTestKind },
}
impl SourceAdmissionRoute {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::SavedConnection { name } | Self::SavedConnectionTest { name, .. } => {
                !name.is_empty() && name.len() <= 512 && name.trim() == name && !name.chars().any(char::is_control)
            }
            Self::UnsavedConnectionTest { invocation_id } => {
                invocation_id.len() == 32
                    && invocation_id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }
        }
    }
    pub fn matches_observation(&self, observation_id: &str) -> bool {
        self.is_valid()
            && match self {
                Self::UnsavedConnectionTest { invocation_id } => invocation_id == observation_id,
                _ => true,
            }
    }
    pub fn saved_connection(&self) -> Option<&str> {
        match self {
            Self::SavedConnection { name } | Self::SavedConnectionTest { name, .. } => Some(name),
            Self::UnsavedConnectionTest { .. } => None,
        }
    }
}
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CollectionAdmissionStatus {
    #[serde(deserialize_with = "required_admission_field")]
    pub observation_id: Option<String>,
    #[serde(deserialize_with = "required_admission_field")]
    pub route: Option<SourceAdmissionRoute>,
    pub source_quiescence_required: bool,
}
fn required_admission_field<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    reader: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(reader)
}
