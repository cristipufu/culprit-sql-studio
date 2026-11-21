//! What is built on a report, adding no findings of its own: the dashboard, the index advisor, the digest for scheduled
//! jobs, the agent brief, drift between two databases, and the waits explained. Each follows the same evidence rules
//! as the checks.

pub(crate) mod advisor;
pub mod backup_history;
pub(crate) mod brief;
pub mod cache_entry_memory;
pub mod cdc;
mod change_capture;
pub mod change_feed;
pub mod cleanup;
pub mod columnstore_rowgroups;
pub mod compilation;
pub mod compiler_admission;
pub mod completed_spills;
pub mod connection_outcomes;
pub mod counter_ratios;
pub mod current_request_work;
pub mod dashboard;
pub(crate) mod digest;
pub mod drift;
pub mod focused_blocking;
pub mod full_text_populations;
pub mod jobs;
pub mod maintenance;
pub mod maintenance_blocking;
pub mod maintenance_progress;
pub mod memory_failures;
mod memory_participants;
pub mod native_failures;
mod native_statistics;
pub mod page_server_reads;
pub mod page_waits;
pub mod performance_counters;
pub(crate) mod plan_advice;
pub mod plan_view;
pub mod query_runtime_fragments;
pub mod query_wait_fragments;
pub mod replication;
pub mod request_memory;
pub mod runtime_distributions;
pub mod session_census;
pub mod session_memory;
pub mod table_memory;
use crate::support::source_coverage;
pub mod source_overhead;
pub mod sql_lint;
pub mod statement_grants;
pub mod statement_work;
pub mod task_space;
pub mod temporary_work;
pub mod tuning;
pub mod wait_counters;
pub mod waits;

pub mod query_store_definitions;

pub mod configured_log_rates;
pub mod database_properties;
pub mod hyperscale_log_components;
pub mod memory_clerks;
pub mod pool_settings;
pub mod suspect_pages;
pub mod table_catalog;
pub mod table_full_text;
pub mod table_temporal;
pub mod tempdb_metadata_memory;

pub mod instance_resources;
pub mod storage;

pub mod group_replication;

pub mod galera;

pub mod managed_instance_resources;
