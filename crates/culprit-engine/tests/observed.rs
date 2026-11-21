pub(crate) async fn run<T>(
    workspace: &culprit_engine::Workspace,
    kind: culprit_core::ProviderReadKind,
    start: impl std::ops::AsyncFnOnce(
        culprit_core::ProviderOperationObservation,
    ) -> culprit_core::Result<culprit_core::ProviderCall<T>>,
) -> culprit_core::Result<T> {
    let owner = workspace.begin_provider_read(kind)?;
    let result = start(owner.operation()?).await;
    owner.finish_call(result).into_result()
}
