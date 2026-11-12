//! AWS credential resolution stays in the native authentication adapter. Exports never reach the CLI's user output.
use super::*;
use culprit_auth::{AccessKeyCredential, AccessKeyProvider};
use serde::Deserialize;
use std::time::{Duration, SystemTime};
use zeroize::{Zeroize, Zeroizing};

const CACHE_LIFETIME: Duration = Duration::from_secs(300);
const EXPIRY_MARGIN: Duration = Duration::from_secs(60);
pub(super) struct CachedKeys {
    credential: AccessKeyCredential,
    refresh_at: SystemTime,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ExportedKeys {
    version: u32,
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    expiration: Option<String>,
}
impl Drop for ExportedKeys {
    fn drop(&mut self) {
        self.access_key_id.zeroize();
        self.secret_access_key.zeroize();
        self.session_token.zeroize();
    }
}
#[async_trait::async_trait]
impl AccessKeyProvider for AwsCli {
    async fn access_keys(&self) -> Result<AccessKeyCredential> {
        let mut cached = self.credentials.lock().await;
        let now = SystemTime::now();
        if let Some(keys) = cached.as_ref().filter(|keys| keys.refresh_at > now) {
            return Ok(keys.credential.clone());
        }
        let bytes = Zeroizing::new(self.run(&["configure", "export-credentials", "--format", "process"]).await?);
        let mut exported: ExportedKeys = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if exported.version != 1
            || !valid_secret(&exported.access_key_id)
            || !valid_secret(&exported.secret_access_key)
            || exported.session_token.as_deref().is_some_and(|token| !valid_secret(token))
        {
            return Err(invalid());
        }
        let expires_at = exported
            .expiration
            .as_deref()
            .map(|text| text.parse::<chrono::DateTime<chrono::Utc>>().map(SystemTime::from).map_err(|_| invalid()))
            .transpose()?;
        if expires_at.is_some_and(|expiry| expiry <= now + EXPIRY_MARGIN) {
            return Err(invalid());
        }
        let refresh_at = expires_at
            .and_then(|expiry| expiry.checked_sub(EXPIRY_MARGIN))
            .map_or(now + CACHE_LIFETIME, |expiry| expiry.min(now + CACHE_LIFETIME));
        let credential = AccessKeyCredential {
            key_id: Zeroizing::new(std::mem::take(&mut exported.access_key_id)),
            secret_key: Zeroizing::new(std::mem::take(&mut exported.secret_access_key)),
            session_token: exported.session_token.take().map(Zeroizing::new),
            expires_at,
        };
        *cached = Some(CachedKeys { credential: credential.clone(), refresh_at });
        Ok(credential)
    }
}
fn valid_secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16384 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn invalid() -> Error {
    Error::new(ErrorCode::AuthFailed, t!("engine.aws.invalid_credentials"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    struct Commands {
        body: Vec<u8>,
        arguments: Mutex<Vec<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl CommandRunner for Commands {
        async fn output(&self, args: &[&str], _: &[(&str, &str)]) -> Result<std::process::Output> {
            self.arguments.lock().unwrap().push(args.iter().map(|s| s.to_string()).collect());
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
            tokio::task::yield_now().await;
            Ok(std::process::Output { status, stdout: self.body.clone(), stderr: vec![] })
        }
    }
    #[tokio::test]
    async fn one_profile_owns_single_flight_resolution_and_expiring_credentials() {
        let commands = Arc::new(Commands { body: serde_json::to_vec(&serde_json::json!({
            "Version":1, "AccessKeyId":"fixture-key", "SecretAccessKey":"private-secret-canary", "SessionToken":"private-session-canary",
            "Expiration": (chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339(),
        })).unwrap(), arguments: Mutex::new(vec![]) });
        let provider = Arc::new(AwsCli::with_profile(commands.clone(), Some("archive-role".into())).unwrap());
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let provider = provider.clone();
            tasks.spawn(async move { provider.access_keys().await.unwrap() });
        }
        while let Some(result) = tasks.join_next().await {
            let credential = result.unwrap();
            assert_eq!(credential.session_token.as_deref().map(|s| s.as_str()), Some("private-session-canary"));
        }
        let calls = commands.arguments.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], ["configure", "export-credentials", "--format", "process", "--profile", "archive-role"]);
    }
    #[tokio::test]
    async fn invalid_or_expired_exports_never_return_or_print_credentials() {
        for body in [
            serde_json::json!({"Version":2,"AccessKeyId":"key","SecretAccessKey":"private-canary"}),
            serde_json::json!({"Version":1,"AccessKeyId":"key","SecretAccessKey":"private-canary","Expiration":"2000-01-01T00:00:00Z"}),
            serde_json::json!({"Version":1,"AccessKeyId":"key","SecretAccessKey":"private-canary","SessionToken":"invalid\nheader"}),
        ] {
            let commands =
                Arc::new(Commands { body: serde_json::to_vec(&body).unwrap(), arguments: Mutex::new(vec![]) });
            let error = AwsCli::new(commands).access_keys().await.err().unwrap();
            assert_eq!(error.code, ErrorCode::AuthFailed);
            assert!(!format!("{error:?}").contains("private-canary"));
        }
    }
}
