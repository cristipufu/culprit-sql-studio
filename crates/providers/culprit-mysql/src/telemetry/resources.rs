//! A hosting instance is discovered once; each native source declares the profiles it actually supports.
use crate::{
    connect::{Connector, Scope},
    native::instance,
    read::{OwnedRead, ReadSession, confirmed_result},
};
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use std::sync::Arc;

pub(crate) async fn discover(
    connector: Arc<Connector>,
    request: &ResourceDiscoveryRequest,
) -> Result<Vec<CollectionResource>> {
    request.budget.validate()?;
    if request.kind != SharedResourceKind::Instance {
        return Err(unsupported());
    }
    let mut reader = ReadSession::new(connector, request.budget.clone(), Scope::Server);
    discover_from(&mut reader, request).await
}

async fn discover_from<R: OwnedRead>(
    reader: &mut R,
    request: &ResourceDiscoveryRequest,
) -> Result<Vec<CollectionResource>> {
    let result = async {
        let (before, collections, _) = identify(reader).await?;
        let (after, after_collections, observed_at) = identify(reader).await?;
        if before != after || collections != after_collections {
            return Err(CollectionStop::SourceChanged.error());
        }
        let resources = vec![CollectionResource { identity: before.resource, collections, observed_at }];
        request.validate_result(&resources)?;
        Ok(resources)
    }
    .await;
    confirmed_result(reader, result).await
}

async fn identify<R: OwnedRead>(reader: &mut R) -> Result<(instance::Identity, Vec<TelemetryCollection>, String)> {
    let platform = instance::platform(reader).await?;
    let collections = super::recipes::instance_collections(&platform);
    if collections.is_empty() {
        return Err(unsupported());
    }
    let row = crate::native::one(reader, "SELECT @@GLOBAL.server_uuid AS server_uuid", vec![]).await?;
    let identity = platform.identify(reader, &row).await?;
    Ok((identity, collections, platform.observed_at))
}

fn unsupported() -> Error {
    Error::new(ErrorCode::Unsupported, culprit_core::message!("collection.unsupported"))
}

#[cfg(test)]
mod tests;
