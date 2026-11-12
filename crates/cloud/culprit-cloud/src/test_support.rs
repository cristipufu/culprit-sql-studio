use crate::*;
struct NoIo;
#[async_trait]
impl culprit_auth::CommandRunner for NoIo {
    async fn output(&self, _: &[&str], _: &[(&str, &str)]) -> Result<std::process::Output> {
        panic!("unexpected command")
    }
}
#[async_trait]
impl culprit_auth::CommandClient for NoIo {
    async fn run(&self, _: &[&str]) -> Result<Vec<u8>> {
        panic!("unexpected native command")
    }
}
#[async_trait]
impl culprit_auth::BearerTokenProvider for NoIo {
    async fn bearer_token(&self, _: &str) -> Result<zeroize::Zeroizing<String>> {
        panic!("unexpected token acquisition")
    }
}
#[async_trait]
impl CloudHttp for NoIo {
    async fn get_bearer(&self, _: &str, _: &str, _: u64) -> Result<JsonReply> {
        panic!("unexpected HTTP")
    }
}
pub fn aws() -> AwsCloud {
    AwsCloud::new(Arc::new(NoIo))
}
pub fn azure() -> AzureCloud {
    AzureCloud::new(Arc::new(NoIo), Arc::new(NoIo), Arc::new(NoIo))
}
