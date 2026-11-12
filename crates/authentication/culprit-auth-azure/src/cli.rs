//! Azure CLI bearer credentials for control-plane APIs. Native audience and output validation stay in this adapter.
use culprit_auth::{BearerTokenProvider, CommandRunner};
use culprit_core::{Error, ErrorCode, Result, t};
use std::sync::Arc;
use zeroize::Zeroizing;

pub struct AzureCliCredential {
    runner: Arc<dyn CommandRunner>,
}
impl AzureCliCredential {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}
#[async_trait::async_trait]
impl BearerTokenProvider for AzureCliCredential {
    async fn bearer_token(&self, resource: &str) -> Result<Zeroizing<String>> {
        if resource != "https://management.azure.com/" {
            return Err(Error::invalid(t!("engine.credentials.invalid_profile")));
        }
        let output = self
            .runner
            .output(
                &["account", "get-access-token", "--resource", resource, "--query", "accessToken", "--output", "tsv"],
                &[],
            )
            .await?;
        if !output.status.success() || output.stdout.is_empty() {
            return Err(Error::new(ErrorCode::AuthFailed, t!("engine.discover.no_arm_token"))
                .with_hint(t!("engine.discover.no_arm_token_hint")));
        }
        let token = Zeroizing::new(
            String::from_utf8(output.stdout)
                .map_err(|_| Error::new(ErrorCode::AuthFailed, t!("engine.discover.unreadable_token")))?,
        );
        if token.trim().is_empty() || token.len() > 32768 || token.trim().bytes().any(|b| !b.is_ascii_graphic()) {
            return Err(Error::new(ErrorCode::AuthFailed, t!("engine.discover.empty_token")));
        }
        Ok(Zeroizing::new(token.trim().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Runner {
        token: &'static [u8],
        calls: std::sync::atomic::AtomicUsize,
    }
    #[async_trait::async_trait]
    impl CommandRunner for Runner {
        async fn output(&self, arguments: &[&str], _: &[(&str, &str)]) -> Result<std::process::Output> {
            assert_eq!(arguments[3], "https://management.azure.com/");
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            #[cfg(unix)]
            let status = {
                use std::os::unix::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            #[cfg(windows)]
            let status = {
                use std::os::windows::process::ExitStatusExt;
                std::process::ExitStatus::from_raw(0)
            };
            Ok(std::process::Output { status, stdout: self.token.to_vec(), stderr: vec![] })
        }
    }
    #[tokio::test]
    async fn control_plane_tokens_validate_the_native_audience_and_sanitize_invalid_output() {
        for token in [b"owned-token\n".as_slice(), b"\n", b"private-canary token", &[0xff]] {
            let runner = Arc::new(Runner { token, calls: std::sync::atomic::AtomicUsize::new(0) });
            let credential = AzureCliCredential::new(runner.clone());
            assert!(credential.bearer_token("https://foreign.invalid/").await.is_err());
            assert_eq!(runner.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            let result = credential.bearer_token("https://management.azure.com/").await;
            if token == b"owned-token\n" {
                assert_eq!(result.unwrap().as_str(), "owned-token");
            } else {
                assert!(!format!("{:?}", result.err().unwrap()).contains("private-canary"));
            }
        }
    }
}
