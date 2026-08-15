use std::{fs, io::Write, path::PathBuf};

use tracing::warn;

/// Append-only action trail: one line per guard decision, reviewable after
/// the fact by the user or their agent.
#[derive(Clone, Debug)]
pub struct ActionLog {
    path: PathBuf,
}

impl ActionLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn record(&self, action: &str) {
        tracing::info!(target: "guard_action", action);
        let line = format!("{action}\n");
        let result = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(err) = result {
            warn!(?err, path = %self.path.display(), "failed to append action log");
        }
    }
}
