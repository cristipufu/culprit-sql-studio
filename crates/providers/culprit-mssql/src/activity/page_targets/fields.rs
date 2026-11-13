//! One native header definition owns both SQL projection and strict decoding. No page-body fields are selected.
use crate::{
    read::{invalid_data, optional_counter, optional_text},
    value::{Rec, Val},
};
use culprit_core::Result;

macro_rules! header_fields {
    ($($field:ident: $kind:ty => $native:literal, $decode:ident;)+) => {
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub(super) struct Header { $(pub $field: $kind,)+ }

        pub(super) fn projection() -> String {
            [$($native,)+].into_iter().map(|name|format!("p.{name}")).collect::<Vec<_>>().join(",")
        }

        impl Header {
            pub(super) fn decode(row: &Rec) -> Result<Self> {
                Ok(Self {$($field:$decode(row,$native)?,)+})
            }
        }

        #[cfg(test)]
        pub(super) const NATIVE_FIELDS: &[&str] = &[$($native,)+];
    }
}

header_fields! {
    database_id: Option<u32> => "database_id", native_integer;
    file_id: Option<u32> => "file_id", native_integer;
    page_id: Option<u32> => "page_id", native_integer;
    header_version: Option<i32> => "page_header_version", signed_integer;
    page_type: Option<i32> => "page_type", signed_integer;
    level: Option<i32> => "page_level", signed_integer;
    object_id: Option<i32> => "object_id", signed_integer;
    index_id: Option<i32> => "index_id", signed_integer;
    partition_id: Option<String> => "partition_id", native_big_id;
    allocation_unit_id: Option<String> => "alloc_unit_id", native_big_id;
    lsn: Option<String> => "page_lsn", native_lsn;
    next_file_id: Option<i32> => "next_page_file_id", signed_integer;
    next_page_id: Option<i32> => "next_page_page_id", signed_integer;
}

pub(super) fn signed_integer(row: &Rec, field: &str) -> Result<Option<i32>> {
    match super::super::rows::value(row, field)? {
        Val::Null => Ok(None),
        Val::Int(value) => i32::try_from(*value).map(Some).map_err(|_| invalid_data()),
        _ => Err(invalid_data()),
    }
}
pub(super) fn native_big_id(row: &Rec, field: &str) -> Result<Option<String>> {
    match super::super::rows::value(row, field)? {
        Val::Null => Ok(None),
        Val::Int(value) => Ok(Some(value.to_string())),
        _ => Err(invalid_data()),
    }
}

fn native_integer(row: &Rec, field: &str) -> Result<Option<u32>> {
    optional_counter(row, field)?
        .map(|value| {
            let value = value.parse::<u32>().map_err(|_| invalid_data())?;
            if value <= i32::MAX as u32 { Ok(value) } else { Err(invalid_data()) }
        })
        .transpose()
}
fn native_lsn(row: &Rec, field: &str) -> Result<Option<String>> {
    let value = optional_text(row, field)?;
    if value.as_ref().is_some_and(|value| value.is_empty() || value.len() > 64 || value.chars().any(char::is_control)) {
        return Err(invalid_data());
    }
    Ok(value)
}
