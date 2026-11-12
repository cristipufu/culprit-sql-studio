//! The store selects physical columns; shared catalog construction owns stream coverage and fingerprints.
pub type SchemaCatalog = culprit_collector::analytical::catalog::PhysicalCatalog<crate::schema::Schema>;
