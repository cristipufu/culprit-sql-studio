//! Persisted full-text catalog metadata. This is not a population-progress or search-health observation.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MssqlFullTextChangeTracking {
    Manual,
    Automatic,
    Off,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlFullTextIndex {
    /// The ordinary unique index SQL Server uses to relate full-text entries to this table's rows.
    pub unique_index_id: Option<u32>,
    pub catalog_id: Option<u32>,
    pub enabled: Option<bool>,
    pub change_tracking: Option<MssqlFullTextChangeTracking>,
    /// The native last-crawl flag; it does not establish current freshness, an empty queue or successful queries.
    pub last_crawl_completed: Option<bool>,
}
impl MssqlFullTextIndex {
    pub fn is_valid(&self) -> bool {
        [self.unique_index_id, self.catalog_id].into_iter().flatten().all(|id| id > 0 && id <= i32::MAX as u32)
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct MssqlTableFullText {
    pub availability: MssqlCatalogAvailability,
    /// `None` establishes no index only when the catalog is available under complete database metadata visibility.
    /// An unavailable catalog never establishes absence. Individual index fields retain native NULLs.
    pub index: Option<MssqlFullTextIndex>,
}
impl MssqlTableFullText {
    pub fn is_valid(&self) -> bool {
        match self.availability {
            MssqlCatalogAvailability::Available => self.index.as_ref().is_none_or(MssqlFullTextIndex::is_valid),
            _ => self.index.is_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> MssqlFullTextIndex {
        MssqlFullTextIndex {
            unique_index_id: Some(1),
            catalog_id: Some(2),
            enabled: Some(false),
            change_tracking: Some(MssqlFullTextChangeTracking::Manual),
            last_crawl_completed: Some(true),
        }
    }

    #[test]
    fn disabled_index_and_native_nulls_round_trip_without_becoming_absence_or_health() {
        for index in [
            index(),
            MssqlFullTextIndex {
                unique_index_id: None,
                catalog_id: None,
                enabled: None,
                change_tracking: None,
                last_crawl_completed: None,
            },
        ] {
            let state = MssqlTableFullText { availability: MssqlCatalogAvailability::Available, index: Some(index) };
            assert!(state.is_valid());
            let bytes = serde_json::to_vec(&state).unwrap();
            assert_eq!(serde_json::from_slice::<MssqlTableFullText>(&bytes).unwrap(), state);
            assert!(state.index.is_some());
        }
    }

    #[test]
    fn unavailable_catalogs_cannot_claim_an_index_and_invalid_native_ids_are_not_repaired() {
        for availability in [
            MssqlCatalogAvailability::Unsupported,
            MssqlCatalogAvailability::PermissionDenied,
            MssqlCatalogAvailability::Unknown,
        ] {
            let mut state = MssqlTableFullText { availability, index: None };
            assert!(state.is_valid());
            state.index = Some(index());
            assert!(!state.is_valid());
        }
        for invalid in [0, i32::MAX as u32 + 1] {
            let mut value = index();
            value.unique_index_id = Some(invalid);
            assert!(!value.is_valid());
            value.unique_index_id = Some(1);
            value.catalog_id = Some(invalid);
            assert!(!value.is_valid());
        }
        let absent = MssqlTableFullText { availability: MssqlCatalogAvailability::Available, index: None };
        assert!(absent.is_valid());
    }
}
