//! Non-secret connection security, resolved from the engine's supported options.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{AuthKind, ConnectionSpec, Error, Result, is_local_host, t};

#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq, Eq)]
#[ts(export)]
pub struct ConnectionSecurity {
    pub mode: Option<String>,
    pub ca_certificate: Option<String>,
    pub trust_server_certificate: bool,
    pub certificate_hostname: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ConnectionSecurityMode {
    pub value: String,
    pub aliases: Vec<String>,
    pub label: String,
    pub description: String,
    pub allows_plaintext: bool,
    pub verifies_identity: bool,
    pub certificate_options: bool,
    pub allows_trust_certificate: bool,
    pub ca_verifies_certificate: bool,
}

impl ConnectionSecurityMode {
    pub fn new(value: &str, label: &str, description: &str) -> Self {
        Self {
            value: value.into(),
            aliases: Vec::new(),
            label: label.into(),
            description: description.into(),
            allows_plaintext: false,
            verifies_identity: false,
            certificate_options: true,
            allows_trust_certificate: false,
            ca_verifies_certificate: false,
        }
    }

    pub fn ca_verifies_certificate(mut self) -> Self {
        self.ca_verifies_certificate = true;
        self
    }

    pub fn aliases(mut self, values: &[&str]) -> Self {
        self.aliases = values.iter().map(|value| (*value).into()).collect();
        self
    }

    pub fn permits_plaintext(mut self) -> Self {
        self.allows_plaintext = true;
        self
    }
    pub fn verifies_identity(mut self) -> Self {
        self.verifies_identity = true;
        self
    }
    pub fn without_certificates(mut self) -> Self {
        self.certificate_options = false;
        self
    }
    pub fn permits_trust_override(mut self) -> Self {
        self.allows_trust_certificate = true;
        self
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ConnectionSecuritySpec {
    pub modes: Vec<ConnectionSecurityMode>,
    pub default_mode: String,
    pub local_default_mode: String,
    pub mode_option: String,
    pub mode_aliases: Vec<String>,
    pub ca_option: String,
    pub ca_aliases: Vec<String>,
    pub trust_certificate_option: Option<String>,
    pub hostname_option: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct ConnectionSecurityInfo {
    pub settings: ConnectionSecurity,
    pub default_mode: String,
    pub effective_mode: String,
    pub requires_verified_tls: bool,
}

fn normalized(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace([' ', '-', '_'], "")
}

fn option(spec: &ConnectionSpec, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| {
            spec.options.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
        })
        .filter(|value| !value.trim().is_empty())
}

impl ConnectionSecuritySpec {
    pub fn mode(&self, value: &str) -> Option<&ConnectionSecurityMode> {
        self.modes.iter().find(|mode| {
            normalized(&mode.value) == normalized(value)
                || mode.aliases.iter().any(|alias| normalized(alias) == normalized(value))
        })
    }

    pub fn describe(&self, spec: &ConnectionSpec) -> ConnectionSecurityInfo {
        let requires_verified_tls = matches!(spec.auth, AuthKind::Entra | AuthKind::AwsIam);
        let default_mode = if !requires_verified_tls && is_local_host(&spec.host) {
            &self.local_default_mode
        } else {
            &self.default_mode
        };
        let mode = option(spec, &self.mode_aliases.iter().map(String::as_str).collect::<Vec<_>>())
            .map(|value| self.mode(&value).map_or(value.clone(), |mode| mode.value.clone()));
        let trust_server_certificate =
            self.trust_certificate_option.as_deref().and_then(|key| spec.flag(key)).unwrap_or(false);
        let selected = mode.as_deref().unwrap_or(default_mode);
        let effective_mode = if requires_verified_tls && self.mode(selected).is_none_or(|mode| !mode.verifies_identity)
        {
            self.default_mode.clone()
        } else {
            selected.into()
        };
        ConnectionSecurityInfo {
            settings: ConnectionSecurity {
                mode,
                ca_certificate: option(spec, &self.ca_aliases.iter().map(String::as_str).collect::<Vec<_>>()),
                trust_server_certificate,
                certificate_hostname: self.hostname_option.as_deref().and_then(|key| option(spec, &[key])),
            },
            default_mode: default_mode.clone(),
            effective_mode,
            requires_verified_tls,
        }
    }

    pub fn apply(&self, spec: &mut ConnectionSpec, settings: &ConnectionSecurity) -> Result<()> {
        let info = self.describe(spec);
        let value = settings.mode.as_deref().unwrap_or(&info.default_mode);
        let mode = self
            .mode(value)
            .ok_or_else(|| Error::invalid(t!("core.connection_security.unknown_mode", mode = value)))?;
        if (settings.trust_server_certificate
            && (!mode.allows_trust_certificate || self.trust_certificate_option.is_none()))
            || (settings.certificate_hostname.is_some() && self.hostname_option.is_none())
        {
            return Err(Error::invalid(t!("core.connection_security.unsupported_certificate_options")));
        }
        if info.requires_verified_tls && (!mode.verifies_identity || settings.trust_server_certificate) {
            return Err(Error::invalid(t!("core.connection_security.verified_required")));
        }
        let mut names = vec![self.mode_option.as_str()];
        names.extend(self.mode_aliases.iter().map(String::as_str));
        names.extend(self.ca_aliases.iter().map(String::as_str));
        names.extend(["sslrootcert", "ssl-ca", "trustServerCertificateCA"]);
        names.extend(self.trust_certificate_option.as_deref());
        names.extend(self.hostname_option.as_deref());
        spec.options.retain(|key, _| !names.iter().any(|name| normalized(key) == normalized(name)));
        if settings.mode.is_some() {
            spec.options.insert(self.mode_option.clone(), mode.value.clone());
        }
        if let Some(path) = settings.ca_certificate.as_deref().filter(|path| !path.trim().is_empty()) {
            spec.options.insert(self.ca_option.clone(), path.trim().into());
        }
        if let Some(key) = &self.trust_certificate_option
            && settings.trust_server_certificate
        {
            spec.options.insert(key.clone(), "true".into());
        }
        if let (Some(key), Some(hostname)) = (&self.hostname_option, &settings.certificate_hostname)
            && !hostname.trim().is_empty()
        {
            spec.options.insert(key.clone(), hostname.trim().into());
        }
        Ok(())
    }
}

pub fn connection_security(spec: &ConnectionSpec) -> Option<ConnectionSecurityInfo> {
    spec.engine?.spec().connection_security().map(|security| security.describe(spec))
}

pub fn apply_connection_security(spec: &mut ConnectionSpec, settings: &ConnectionSecurity) -> Result<()> {
    let security = spec.engine.and_then(|engine| engine.spec().connection_security());
    match security {
        Some(security) => security.apply(spec, settings),
        None if settings == &ConnectionSecurity::default() => Ok(()),
        None => Err(Error::invalid(t!("core.connection_security.not_applicable"))),
    }
}

#[cfg(test)]
mod tests;
