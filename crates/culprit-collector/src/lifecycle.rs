//! Await owned operations through cancellation; neither lifetime is abandoned.
use crate::capture::CollectionCancellation;
use culprit_core::Result;
use std::future::Future;

/// Retain both operation owners through shutdown. Completion or failure stops new work and awaits
/// the other operation; the primary operation's native cancellation error takes priority.
pub async fn with_background<T>(
    cancellation: &CollectionCancellation,
    source: impl Future<Output = Result<T>>,
    delivery: impl Future<Output = Result<()>>,
) -> Result<T> {
    tokio::pin!(source, delivery);
    let (source_result, delivery_result) = tokio::select! {
        result=&mut source=>{
            cancellation.cancel();
            (result,delivery.await)
        },
        result=&mut delivery=>{
            cancellation.cancel();
            (source.await,result)
        },
    };
    // Source cancellation uncertainty has priority; it must remain visible even if delivery also failed.
    let summary = source_result?;
    delivery_result?;
    Ok(summary)
}
