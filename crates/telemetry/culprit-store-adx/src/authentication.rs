//! Public service metadata is validated before requesting a token from the embedding runtime.
pub use culprit_auth::BearerTokenProvider as TokenProvider;
use culprit_core::{Result, telemetry::DeliveryProblem};

#[derive(Clone)]
pub struct Audience(String);
impl Audience {
    pub fn kusto(endpoint: &str, advertised_resource: &str) -> Result<Self> {
        let origin = kusto_origin(endpoint)?;
        if advertised_resource != origin
            && !matches!(advertised_resource, "https://kusto.kusto.windows.net" | "https://api.kusto.windows.net")
        {
            return Err(configuration_error());
        }
        Ok(Self(advertised_resource.into()))
    }
    pub fn resource(&self) -> &str {
        &self.0
    }
}
pub(crate) fn kusto_origin(endpoint: &str) -> Result<String> {
    let origin = culprit_auth::https_origin(endpoint).ok_or_else(configuration_error)?;
    if !origin.ends_with(".kusto.windows.net") {
        return Err(configuration_error());
    }
    Ok(origin)
}
fn configuration_error() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authentication_metadata_cannot_redirect_credentials_to_another_origin() {
        for endpoint in [
            "https://x.kusto.windows.net@attacker.example",
            "http://x.kusto.windows.net",
            "https://x.kusto.windows.net/path",
            "https://x.kusto.windows.net?token=secret",
        ] {
            assert!(Audience::kusto(endpoint, "https://kusto.kusto.windows.net").is_err());
        }
        assert_eq!(
            Audience::kusto(
                "https://cluster.westeurope.kusto.windows.net/",
                "https://cluster.westeurope.kusto.windows.net"
            )
            .unwrap()
            .resource(),
            "https://cluster.westeurope.kusto.windows.net"
        );
        assert_eq!(
            Audience::kusto("https://cluster.westeurope.kusto.windows.net", "https://kusto.kusto.windows.net")
                .unwrap()
                .resource(),
            "https://kusto.kusto.windows.net"
        );
        for advertised in [
            "https://attacker.example",
            "http://kusto.kusto.windows.net",
            "https://different.kusto.windows.net",
            "https://kusto.kusto.windows.net/.default",
        ] {
            assert!(Audience::kusto("https://cluster.westeurope.kusto.windows.net", advertised).is_err());
        }
    }
}
