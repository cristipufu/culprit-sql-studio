//! Activity and page lookups reuse the shared native source authority without creating another reader.
use super::ActivityReader;
pub(crate) use crate::local_source::{Context, Detail};
use crate::{read::NativeRead, value::Rec};
use culprit_core::Result;

pub(crate) async fn initial<R: NativeRead>(reader: &mut ActivityReader<R>) -> Result<Context> {
    read_initial(reader, Detail::Activity).await.map(|(context, _)| context)
}
pub(crate) async fn recheck<R: NativeRead>(reader: &mut ActivityReader<R>, initial: &Context) -> Result<Context> {
    read_again(reader, initial, Detail::Activity).await.map(|(context, _)| context)
}
pub(super) async fn read_initial<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    detail: Detail,
) -> Result<(Context, Rec)> {
    let result =
        crate::local_source::read_initial(&mut reader.io, &reader.host, reader.port, reader.options.identity, detail)
            .await?;
    reader.request_page_server_read_capability = result.0.activity.request_page_server_read_capability;
    Ok(result)
}
pub(super) async fn read_again<R: NativeRead>(
    reader: &mut ActivityReader<R>,
    initial: &Context,
    detail: Detail,
) -> Result<(Context, Rec)> {
    crate::local_source::read_again(&mut reader.io, &reader.host, reader.port, initial, detail).await
}
#[cfg(test)]
pub(crate) fn test_azure_authority(row: &Rec) -> Result<Context> {
    crate::local_source::test_azure_authority(row)
}
#[cfg(test)]
pub(crate) mod tests;
