use crate::models::DownloadState;
use anyhow::{Context, Result};
use serde_json;
use std::fs;
use std::path::PathBuf;

pub struct StateManager {
    state_file: PathBuf,
}

impl StateManager {
    pub fn new(output_dir: &PathBuf) -> Self {
        let state_file = output_dir.join(".download_state.json");
        Self { state_file }
    }

    pub fn load_state(&self) -> Result<Vec<DownloadState>> {
        if !self.state_file.exists() {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.state_file)
            .context("Failed to read state file")?;

        serde_json::from_str(&content).context("Failed to parse state file")
    }

    pub fn save_state(&self, states: &[DownloadState]) -> Result<()> {
        let content = serde_json::to_string_pretty(states)
            .context("Failed to serialize state")?;

        fs::write(&self.state_file, content).context("Failed to write state file")?;

        Ok(())
    }
}
