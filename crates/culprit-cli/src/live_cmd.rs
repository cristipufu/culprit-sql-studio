//! Administrative signals use the original saved observation. A fresh lookup cannot recover the generation
//! the person intended when they selected a reusable numeric session ID.
use crate::{LiveArgs, output::Out};
use culprit_core::{Error, LiveState, Result, SessionAction, SessionActionRequest, SessionActionTarget, t};
use culprit_engine::Workspace;
use std::{io::Read, path::Path};

const MAX_OBSERVATION_BYTES: u64 = 16 * 1024 * 1024;

pub(super) async fn run(ws: &Workspace, args: LiveArgs, out: &Out) -> Result<u8> {
    let target = match (args.cancel, args.kill) {
        (Some(id), _) => Some((id, SessionAction::Cancel)),
        (None, Some(id)) => Some((id, SessionAction::Terminate)),
        (None, None) => None,
    };
    if let Some((id, action)) = target {
        let path = args.observation.as_deref().ok_or_else(SessionActionTarget::unavailable)?;
        let observed = read_observation(path)?;
        let request = action_request(&args.connection, &observed, &id, action)?;
        let outcome = ws.interrupt_session(&request).await?;
        out.session_signal(&id, outcome);
        return Ok(0);
    }
    if let Some(session) = args.progress.as_deref() {
        let mut first = true;
        loop {
            let progress = ws.query_progress(&args.connection, session, first || !args.watch).await?;
            out.query_progress(&progress, !first);
            if !args.watch || progress.running == Some(false) {
                return Ok(0);
            }
            first = false;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    out.live(&ws.live(&args.connection).await?);
    Ok(0)
}

fn action_request(name: &str, observed: &LiveState, id: &str, action: SessionAction) -> Result<SessionActionRequest> {
    let request = observed.action_request(id, action)?;
    if request.route.connection != name {
        return Err(Error::invalid(t!("cli.live.wrong_observation", connection = name)));
    }
    Ok(request)
}

fn read_observation(path: &Path) -> Result<LiveState> {
    let unreadable = || Error::invalid(t!("cli.live.observation_unreadable"));
    let file = std::fs::File::open(path).map_err(|_| unreadable())?;
    let metadata = file.metadata().map_err(|_| unreadable())?;
    if !metadata.is_file() || metadata.len() > MAX_OBSERVATION_BYTES {
        return Err(unreadable());
    }
    let mut bytes = Vec::new();
    file.take(MAX_OBSERVATION_BYTES + 1).read_to_end(&mut bytes).map_err(|_| unreadable())?;
    if bytes.len() as u64 > MAX_OBSERVATION_BYTES {
        return Err(unreadable());
    }
    serde_json::from_slice(&bytes).map_err(|_| unreadable())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn session_ids_alone_cannot_send_signals_and_progress_stays_independent() {
        for action in ["--cancel", "--kill"] {
            assert!(Cli::try_parse_from(["culprit", "live", "dev", action, "42"]).is_err());
            assert!(
                Cli::try_parse_from(["culprit", "live", "dev", action, "42", "--observation", "observed.json"]).is_ok()
            );
        }
        assert!(Cli::try_parse_from(["culprit", "live", "dev"]).is_ok());
        assert!(Cli::try_parse_from(["culprit", "live", "dev", "--progress", "42", "--watch"]).is_ok());
        assert!(Cli::try_parse_from(["culprit", "live", "dev", "--observation", "observed.json"]).is_err());
        assert!(
            Cli::try_parse_from([
                "culprit",
                "live",
                "dev",
                "--cancel",
                "42",
                "--kill",
                "43",
                "--observation",
                "observed.json"
            ])
            .is_err()
        );
    }

    #[test]
    fn invalid_or_oversized_observations_expose_no_native_document() {
        let path = std::env::temp_dir().join(format!("culprit-live-observation-{}", std::process::id()));
        std::fs::write(&path, b"private-invalid-observation-canary").unwrap();
        let error = read_observation(&path).unwrap_err();
        assert!(!error.message.contains("canary"));
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_OBSERVATION_BYTES + 1).unwrap();
        assert!(read_observation(&path).is_err());
        drop(file);
        std::fs::remove_file(path).unwrap();
    }
}
