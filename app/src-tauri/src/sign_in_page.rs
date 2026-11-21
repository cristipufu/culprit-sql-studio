//! Fixed native browser openers receive only the pending identity-provider page, never a webview command.
use culprit_auth::DeviceCodePrompt;
use culprit_core::{Error, Result, format, message};
use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    sync::Mutex,
};

/// Microsoft's device-login pages only. Keep this policy independent of platform command execution.
fn allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    matches!(
        host.as_str(),
        "microsoft.com" | "www.microsoft.com" | "login.microsoftonline.com" | "login.microsoft.com" | "aka.ms"
    )
}

pub(super) fn open_pending(prompt: &Mutex<Option<DeviceCodePrompt>>) -> Result<()> {
    open_with(prompt, launch)
}

fn open_with(
    prompt: &Mutex<Option<DeviceCodePrompt>>,
    launcher: impl FnOnce(&str) -> io::Result<ExitStatus>,
) -> Result<()> {
    let url = prompt
        .lock()
        .ok()
        .and_then(|pending| pending.as_ref().map(|pending| pending.verification_uri.clone()))
        .filter(|url| allowed(url))
        .ok_or_else(|| Error::not_found(message!("ui.sign_in_dialog.no_pending")))?;
    opened(launcher(&url))
}

fn launch(url: &str) -> io::Result<ExitStatus> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    // One direct argument, no shell. status owns and waits for the existing launcher; no detached worker or retry.
    // Browser interaction uses its GUI. Launcher output is not forwarded to Culprit's input/output streams.
    Command::new(program).arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()
}

fn opened(result: io::Result<ExitStatus>) -> Result<()> {
    let status = result
        .map_err(|error| Error::internal(message!("ui.sign_in_dialog.browser_error", error = error.to_string())))?;
    if status.success() {
        return Ok(());
    }
    let reason = match status.code() {
        Some(code) => message!("ui.sign_in_dialog.browser_exit", code = format::digits(code.to_string())),
        None => message!("ui.sign_in_dialog.browser_stopped"),
    };
    Err(Error::internal(reason))
}

#[cfg(test)]
mod tests;
