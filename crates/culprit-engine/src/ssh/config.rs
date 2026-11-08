use culprit_core::{Engine, Error, Result, SshConfig, t};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Target {
    pub host: String,
    pub port: u16,
}

fn host_name(host: &str) -> Option<&str> {
    if let Some(raw) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return raw.parse::<std::net::Ipv6Addr>().is_ok().then_some(raw);
    }
    (host.parse::<std::net::IpAddr>().is_ok()
        || (!host.is_empty()
            && host.len() <= 253
            && !host.starts_with('-')
            && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))))
    .then_some(host)
}

impl Target {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        let host = host_name(host.trim()).filter(|_| port != 0 && !host.chars().any(char::is_control));
        let Some(host) = host else {
            return Err(Error::invalid(t!("engine.ssh.bad_target")));
        };
        Ok(Self { host: host.into(), port })
    }

    pub fn forwarding(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Validate before persisting or resolving database credentials. Never let SSH turn an input into shell syntax,
/// including through `%h` / `%r` expansion in a trusted ProxyCommand configuration.
pub(crate) fn normalize(mut ssh: SshConfig, engine: Engine) -> Result<SshConfig> {
    if !engine.has_server() {
        return Err(Error::invalid(t!("engine.ssh.file_database")));
    }
    let input = ssh.destination.trim().strip_prefix("ssh://").unwrap_or(ssh.destination.trim());
    let (user, host) = input.rsplit_once('@').map_or((None, input), |(user, host)| (Some(user), host));
    if let Some(user) = user
        && (user.is_empty()
            || user.len() > 256
            || user.starts_with('-')
            || !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '\\')))
    {
        return Err(Error::invalid(t!("engine.ssh.bad_user")));
    }
    let (host, embedded_port) = if host.starts_with('[') {
        let end = host.find(']').ok_or_else(|| Error::invalid(t!("engine.ssh.unclosed_ipv6")))?;
        let rest = &host[end + 1..];
        (&host[..=end], (!rest.is_empty()).then_some(rest))
    } else if host.matches(':').count() == 1
        && let Some(separator) = host.find(':')
    {
        (&host[..separator], Some(&host[separator..]))
    } else {
        (host, None)
    };
    let host = host_name(host)
        .ok_or_else(|| Error::invalid(t!("engine.ssh.bad_host")).with_hint(t!("engine.ssh.bad_host_hint")))?;
    if let Some(port) = embedded_port {
        let port: u16 = port
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .filter(|p| *p > 0)
            .ok_or_else(|| Error::invalid(t!("engine.ssh.bad_port")))?;
        if ssh.port.is_some_and(|explicit| explicit != port) {
            return Err(Error::invalid(t!("engine.ssh.ports_differ")));
        }
        ssh.port = Some(port);
    }
    if ssh.port == Some(0) {
        return Err(Error::invalid(t!("engine.ssh.bad_port")));
    }
    ssh.destination = match user {
        Some(user) => format!("{user}@{host}"),
        None => host.into(),
    };
    ssh.identity_file = ssh
        .identity_file
        .filter(|key| !key.trim().is_empty())
        .map(|key| {
            if key.len() > 4096 || key.chars().any(char::is_control) {
                return Err(Error::invalid(t!("engine.ssh.bad_identity_path")));
            }
            let path = match key.trim().strip_prefix("~/").or_else(|| key.trim().strip_prefix("~\\")) {
                Some(rest) => dirs::home_dir().ok_or_else(|| Error::not_found(t!("engine.ssh.no_home")))?.join(rest),
                None => std::path::PathBuf::from(key.trim()),
            };
            let path = std::path::absolute(path)
                .map_err(|_| Error::invalid(t!("engine.ssh.unresolved_identity")))?
                .to_string_lossy()
                .into_owned();
            // Home/cwd expansion can introduce tokens even when the entered path contains none.
            if path.contains('%') {
                return Err(Error::invalid(t!("engine.ssh.identity_expansion"))
                    .with_hint(t!("engine.ssh.identity_expansion_hint")));
            }
            Ok(path)
        })
        .transpose()?;
    Ok(ssh)
}
