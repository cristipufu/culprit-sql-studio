use super::*;
use culprit_core::ErrorCode;

fn prompt(url: &str) -> Mutex<Option<DeviceCodePrompt>> {
    Mutex::new(Some(DeviceCodePrompt {
        user_code: "private-device-code".into(),
        verification_uri: url.into(),
        message: "native provider instruction".into(),
        expires_in_secs: 300,
    }))
}
fn status(code: u32) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw((code as i32) << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code)
    }
}

#[test]
fn only_an_existing_allowed_pending_page_can_reach_one_direct_launcher_call() {
    for url in [
        "https://microsoft.com/devicelogin",
        "https://WWW.MICROSOFT.COM/",
        "https://login.microsoftonline.com/common",
        "https://login.microsoft.com/",
        "https://aka.ms/devicelogin",
    ] {
        let calls = std::cell::Cell::new(0);
        let prompt = prompt(url);
        open_with(&prompt, |observed| {
            calls.set(calls.get() + 1);
            assert_eq!(observed, url);
            Ok(status(0))
        })
        .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(prompt.lock().unwrap().as_ref().unwrap().verification_uri, url);
    }
    for url in [
        "http://microsoft.com/",
        "https://microsoft.com.evil.invalid/",
        "https://microsoft.com@evil.invalid/",
        "https://microsoft.com:443/",
        "https://microsoft.com\\@evil.invalid/",
        "https://evil.invalid/",
        "file:///private",
        ";not-a-command",
    ] {
        let error = open_with(&prompt(url), |_| panic!("refused URI launched a process")).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.message_source().render(), culprit_core::t!("ui.sign_in_dialog.no_pending"));
        assert!(!error.message.contains(url));
    }
    let error = culprit_core::i18n::scope("en", || {
        open_with(&Mutex::new(None), |_| panic!("no pending page launched a process")).unwrap_err()
    });
    assert_eq!(error.message, "No sign-in is waiting");
}

#[test]
fn launcher_spawn_failure_keeps_existing_english_and_does_not_consume_the_pending_prompt() {
    culprit_core::i18n::scope("en", || {
        let prompt = prompt("https://microsoft.com/devicelogin");
        let error =
            open_with(&prompt, |_| Err(io::Error::new(io::ErrorKind::NotFound, "opener unavailable"))).unwrap_err();
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(error.message, "Could not open the browser: opener unavailable");
        assert!(error.detail.is_none() && error.hint.is_none());
        assert!(prompt.lock().unwrap().is_some());
    });
}

#[test]
fn a_failed_native_exit_is_not_success_and_keeps_the_original_status_without_returning_the_uri() {
    let url = "https://microsoft.com/devicelogin?private-route-canary";
    let error = culprit_core::i18n::scope("en", || open_with(&prompt(url), |_| Ok(status(7))).unwrap_err());
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.message, "Could not open the browser (exit code 7).");
    let json = serde_json::to_string(&error).unwrap();
    assert!(!json.contains("private-route-canary") && !json.contains("private-device-code"));
    for language in ["es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            assert_eq!(
                error.message_source().render(),
                culprit_core::t!("ui.sign_in_dialog.browser_exit", code = culprit_core::format::digits("7".into()))
            );
            assert_ne!(error.message_source().render(), error.message);
        });
    }
}

#[cfg(unix)]
#[test]
fn a_launcher_terminated_by_signal_cannot_report_success_or_an_invented_exit_code() {
    use std::os::unix::process::ExitStatusExt;
    let error = culprit_core::i18n::scope("en", || opened(Ok(ExitStatus::from_raw(9))).unwrap_err());
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.message, "Could not open the browser because the opener stopped without an exit code.");
}

#[test]
fn nearby_desktop_errors_keep_existing_english_bytes_when_the_words_move_to_catalogs() {
    culprit_core::i18n::scope("en", || {
        for (message, expected) in [
            (message!("engine.brief.no_finding", id = "finding-7"), "No finding finding-7"),
            (message!("engine.no_saved_report", name = "sample"), "No saved report for 'sample'"),
            (message!("engine.scan_first", name = "sample"), "Run `culprit scan sample` first"),
            (message!("ui.ssh_fields.identity_unreadable"), "That identity file cannot be read from here"),
            (
                message!("ui.updates.check_failed", error = "native failure"),
                "Couldn't check for updates: native failure",
            ),
            (message!("ui.updates.endpoint_invalid"), "CULPRIT_UPDATE_ENDPOINT isn't a URL"),
            (message!("ui.updates.no_pending"), "No update is waiting. Check for updates again."),
            (
                message!("ui.updates.install_failed", error = "native failure"),
                "Couldn't install the update: native failure",
            ),
        ] {
            assert_eq!(message.render(), expected);
        }
    });
}
