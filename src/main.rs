mod cli;
mod downloader;
mod models;
mod state;

use anyhow::{anyhow, Result};
use clap::Parser;
use cli::{Args, DownloadMode};
use downloader::Downloader;
use std::path::Path;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let link_file = args.link_file.unwrap_or_else(|| "sa-1b_link.txt".to_string());

    if !Path::new(&link_file).exists() {
        return Err(anyhow!("Link file not found: {}", link_file));
    }

    let downloader = Downloader::new(&args.output, args.resume, args.proxy.as_deref(), args.retries)?;

    let mut entries = downloader.parse_link_file(&link_file)?;

    if entries.is_empty() {
        return Err(anyhow!("No entries found in link file"));
    }

    entries.sort_by(|a, b| a.file_name.cmp(&b.file_name));

    println!("Loaded {} entries from {}", entries.len(), link_file);

    match args.mode {
        DownloadMode::All => {
            downloader.download_all(entries.clone(), args.threads).await?;
        }
        DownloadMode::Single => {
            let file_name = args.file.ok_or_else(|| anyhow!("--file argument required for single mode"))?;
            let entry = entries
                .iter()
                .find(|e| e.file_name == file_name)
                .ok_or_else(|| anyhow!("File not found in link file: {}", file_name))?
                .clone();

            downloader.download_single(&entry).await?;
        }
        DownloadMode::Range => {
            let start = args.start.ok_or_else(|| anyhow!("--start argument required for range mode"))?;
            let end = args.end.ok_or_else(|| anyhow!("--end argument required for range mode"))?;
            if start >= entries.len() || end >= entries.len() || start > end {
                return Err(anyhow!("Invalid range: start={}, end={}, total={}", start, end, entries.len()));
            }
            let range_entries = entries[start..=end].to_vec();
            println!("Downloading files from index {} to {} ({} files)", start, end, range_entries.len());
            downloader.download_all(range_entries, args.threads).await?;
        }
    }

    Ok(())
}
