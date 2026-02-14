use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkEntry {
    pub file_name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadState {
    pub file_name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub completed: bool,
}

impl DownloadState {
    pub fn new(file_name: String) -> Self {
        Self {
            file_name,
            downloaded_bytes: 0,
            total_bytes: None,
            completed: false,
        }
    }
}
