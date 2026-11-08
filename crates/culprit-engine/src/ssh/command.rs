use std::process::Stdio;

use culprit_core::{Error, ErrorCode, Result, SshConfig, t};
use tokio::process::Command;

use super::config::Target;
use crate::program::{self, Cli};

pub(super) fn command(ssh: &SshConfig, target: &Target) -> Result<Command> {
    Ok(configure(program::command(Cli::Ssh)?, ssh, target))
}

pub(super) fn configure(mut command: Command, ssh: &SshConfig, target: &Target) -> Command {
    command.args([
        "-T",
        "-a",
        "-x",
        "-k",
        "-oBatchMode=yes",
        "-oStrictHostKeyChecking=yes",
        "-oUpdateHostKeys=no",
        "-oClearAllForwardings=yes",
        "-oExitOnForwardFailure=yes",
        "-oPermitLocalCommand=no",
        "-oForkAfterAuthentication=no",
        "-oControlMaster=no",
        "-oControlPersist=no",
        "-oStdinNull=no",
        "-oRemoteCommand=none",
        "-oConnectionAttempts=1",
        "-oConnectTimeout=10",
        "-oServerAliveInterval=15",
        "-oServerAliveCountMax=2",
        "-oLogLevel=ERROR",
    ]);
    if let Some(port) = ssh.port {
        command.arg("-p").arg(port.to_string());
    }
    if let Some(path) = &ssh.identity_file {
        command.arg("-i").arg(path);
    }
    command.arg("-W").arg(target.forwarding()).arg("--").arg(&ssh.destination);
    command.env("SSH_ASKPASS_REQUIRE", "never").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

pub(super) fn failure(stderr: &[u8]) -> Error {
    let message = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if message.contains("bad configuration option") || message.contains("unsupported option") {
        return Error::new(ErrorCode::Unsupported, t!("engine.ssh.unsupported_options"))
            .with_hint(t!("engine.ssh.unsupported_options_hint"));
    }
    let (message, hint) = if message.contains("host key") || message.contains("host identification has changed") {
        (t!("engine.ssh.host_key"), t!("engine.ssh.host_key_hint"))
    } else if message.contains("permission denied")
        || message.contains("signing failed")
        || message.contains("load key")
        || message.contains("identity file")
    {
        (t!("engine.ssh.auth_failed"), t!("engine.ssh.auth_failed_hint"))
    } else if message.contains("administratively prohibited") || message.contains("forwarding disabled") {
        (t!("engine.ssh.forwarding_refused"), t!("engine.ssh.forwarding_refused_hint"))
    } else if message.contains("connect failed")
        || message.contains("connection refused")
        || message.contains("could not resolve")
        || message.contains("timed out")
    {
        (t!("engine.ssh.unreachable"), t!("engine.ssh.unreachable_hint"))
    } else {
        (t!("engine.ssh.failed"), t!("engine.ssh.failed_hint"))
    };
    Error::new(ErrorCode::ConnectionFailed, message).with_hint(hint)
}
