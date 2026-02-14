use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum DownloadMode {
    All,
    Single,
    Range,
}

#[derive(Parser, Debug)]
#[command(name = "sa-1b-dl")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Link file path (default: sa-1b_link.txt)
    #[arg(short, long)]
    link_file: Option<String>,

    /// Output directory
    #[arg(short, long, default_value = "./my_downloads")]
    output: String,

    /// Download mode
    #[arg(short, long, value_enum, default_value = "all")]
    mode: DownloadMode,

    /// Single file to download (file name from link file)
    #[arg(short = 'f', long)]
    file: Option<String>,

    /// Start index for range download (inclusive)
    #[arg(long, requires = "end")]
    start: Option<usize>,

    /// End index for range download (inclusive)
    #[arg(long, requires = "start")]
    end: Option<usize>,

    /// Number of parallel downloads
    #[arg(short, long, default_value = "4")]
    threads: usize,

    /// Resume interrupted downloads
    #[arg(long, default_value = "true")]
    resume: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LinkEntry {
    file_name: String,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadState {
    file_name: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    completed: bool,
}

impl DownloadState {
    fn new(file_name: String) -> Self {
        Self {
            file_name,
            downloaded_bytes: 0,
            total_bytes: None,
            completed: false,
        }
    }
}

struct Downloader {
    client: Client,
    output_dir: PathBuf,
    state_file: PathBuf,
    resume: bool,
}

impl Downloader {
    fn new(output_dir: &str, resume: bool) -> Result<Self> {
        let output_path = PathBuf::from(output_dir);
        if !output_path.exists() {
            fs::create_dir_all(&output_path)?;
        }

        let state_file = output_path.join(".download_state.json");

        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            output_dir: output_path,
            state_file,
            resume,
        })
    }

    fn load_state(&self) -> Result<Vec<DownloadState>> {
        if !self.state_file.exists() || !self.resume {
            return Ok(Vec::new());
        }

        let content = fs::read_to_string(&self.state_file)
            .context("Failed to read state file")?;

        serde_json::from_str(&content).context("Failed to parse state file")
    }

    fn save_state(&self, states: &[DownloadState]) -> Result<()> {
        let content = serde_json::to_string_pretty(states)
            .context("Failed to serialize state")?;

        fs::write(&self.state_file, content).context("Failed to write state file")?;

        Ok(())
    }

    fn parse_link_file(&self, path: &str) -> Result<Vec<LinkEntry>> {
        let file = File::open(path).context("Failed to open link file")?;
        let reader = BufReader::new(file);

        let mut entries = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let line = line.context("Failed to read line")?;

            // Skip header
            if idx == 0 && line.starts_with("file_name") {
                continue;
            }

            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() >= 2 {
                entries.push(LinkEntry {
                    file_name: parts[0].to_string(),
                    url: parts[1].to_string().trim().to_string(),
                });
            }
        }

        Ok(entries)
    }

    async fn download_file(
        &self,
        entry: &LinkEntry,
        state: Arc<Mutex<DownloadState>>,
        pb: &ProgressBar,
    ) -> Result<()> {
        let output_path = self.output_dir.join(&entry.file_name);
        let partial_path = format!("{}.part", output_path.display());

        // Make HEAD request to get file size
        let response = self
            .client
            .head(&entry.url)
            .send()
            .await
            .context("HEAD request failed")?;

        let total_bytes = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());

        // Check if already completed
        if output_path.exists() {
            let actual_size = fs::metadata(&output_path)?.len();
            let is_valid = if let Some(expected) = total_bytes {
                actual_size == expected
            } else {
                true
            };

            {
                let mut state = state.lock().unwrap();
                state.completed = true;
                state.downloaded_bytes = actual_size;
            }

            if is_valid {
                pb.set_message("Skipped (valid)");
            } else {
                pb.set_message("Skipped (size mismatch!)");
            }
            pb.finish();

            if !is_valid {
                return Err(anyhow!(
                    "Existing file size mismatch for {}: expected {} bytes, got {} bytes",
                    entry.file_name,
                    total_bytes.unwrap_or(0),
                    actual_size
                ));
            }
            return Ok(());
        }

        // Get current position for resume
        let mut current_pos = 0u64;
        if self.resume && Path::new(&partial_path).exists() {
            current_pos = fs::metadata(&partial_path)?.len();
        }

        // Validate partial file size against expected total
        if let Some(total) = total_bytes {
            if current_pos > 0 && current_pos > total {
                // Partial file is larger than expected, something is wrong
                return Err(anyhow!(
                    "Partial file size {} exceeds expected size {} for {}",
                    current_pos, total, entry.file_name
                ));
            }
        }

        {
            let mut state = state.lock().unwrap();
            state.total_bytes = total_bytes;
        }

        if let Some(total) = total_bytes {
            if current_pos >= total {
                fs::rename(&partial_path, &output_path)
                    .context("Failed to rename completed file")?;

                // Verify file size
                let actual_size = fs::metadata(&output_path)?.len();
                let is_valid = actual_size == total;

                {
                    let mut state = state.lock().unwrap();
                    state.completed = true;
                    state.downloaded_bytes = actual_size;
                }

                if is_valid {
                    pb.set_message("Done (resumed)");
                } else {
                    pb.set_message("Size mismatch!");
                }
                pb.finish();

                if !is_valid {
                    // Delete the invalid file so it can be re-downloaded
                    let _ = fs::remove_file(&output_path);
                    return Err(anyhow!(
                        "File size mismatch for {}: expected {} bytes, got {} bytes. File deleted for re-download.",
                        entry.file_name,
                        total,
                        actual_size
                    ));
                }
                return Ok(());
            }

            // Configure progress bar
            pb.set_length(total);
            pb.set_position(current_pos);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{msg:30} {bar:40} {bytes}/{total_bytes} ({bytes_per_sec})")
                    .unwrap()
                    .progress_chars("=>-"),
            );
        }

        // Open file for writing
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial_path)
            .context("Failed to open output file")?;

        // Create request with range header if resuming
        let mut request = self.client.get(&entry.url);
        if current_pos > 0 {
            request = request.header("Range", format!("bytes={}-", current_pos));
        }

        let mut response = request.send().await.context("GET request failed")?;

        if !response.status().is_success() && response.status() != 206 {
            return Err(anyhow!("HTTP request failed: {}", response.status()));
        }

        let mut downloaded = 0u64;

        while let Some(chunk) = response.chunk().await? {
            let n = chunk.len();
            if n == 0 {
                break;
            }
            file.write_all(&chunk)?;
            downloaded += n as u64;

            let total_downloaded = current_pos + downloaded;
            {
                let mut state = state.lock().unwrap();
                state.downloaded_bytes = total_downloaded;
            }
            if total_bytes.is_some() {
                pb.set_position(total_downloaded);
            }
        }

        // Rename to completed
        fs::rename(&partial_path, &output_path)?;

        // Verify file integrity by checking size
        let actual_size = fs::metadata(&output_path)?.len();
        let expected_size = total_bytes.unwrap_or(0);

        let is_valid = if expected_size > 0 {
            actual_size == expected_size
        } else {
            true // No expected size to compare
        };

        {
            let mut state = state.lock().unwrap();
            state.completed = true;
            state.downloaded_bytes = actual_size;
        }

        if is_valid {
            pb.set_message("Done");
        } else {
            pb.set_message("Size mismatch!");
        }
        pb.finish();

        if !is_valid {
            return Err(anyhow!(
                "File size mismatch for {}: expected {} bytes, got {} bytes",
                entry.file_name,
                expected_size,
                actual_size
            ));
        }

        Ok(())
    }

    async fn download_all(&self, entries: Vec<LinkEntry>, num_threads: usize) -> Result<()> {
        let states: Arc<Mutex<Vec<DownloadState>>> = Arc::new(Mutex::new(self.load_state()?));
        let mp = Arc::new(MultiProgress::new());

        // Create progress bar for overall progress
        let overall_pb = Arc::new(mp.add(ProgressBar::new(entries.len() as u64)));
        overall_pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} {bar:40} {pos}/{len}")
                .unwrap()
                .progress_chars("=>-"),
        );
        overall_pb.set_message("Overall");

        // Semaphore to limit concurrent downloads
        let semaphore = Arc::new(Semaphore::new(num_threads));

        let mut handles = Vec::new();

        for (idx, entry) in entries.iter().enumerate() {
            let entry = entry.clone();
            let state = {
                let states = states.lock().unwrap();
                states
                    .iter()
                    .find(|s| s.file_name == entry.file_name)
                    .cloned()
                    .unwrap_or_else(|| DownloadState::new(entry.file_name.clone()))
            };
            let state = Arc::new(Mutex::new(state));
            let states = Arc::clone(&states);
            let semaphore = Arc::clone(&semaphore);
            let mp = mp.clone();
            let overall_pb = Arc::clone(&overall_pb);
            let downloader = self.clone();

            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let handle = task::spawn(async move {
                // Create progress bar inside the task (after semaphore acquired)
                let pb = mp.add(ProgressBar::new(100));
                let msg = format!("[{:>2}] {}", idx, entry.file_name);
                pb.set_message(msg);
                pb.set_style(
                    ProgressStyle::default_bar()
                        .template("{msg}")
                        .unwrap()
                        .progress_chars("=>-"),
                );

                let result = downloader.download_file(&entry, Arc::clone(&state), &pb).await;

                // Update shared state
                {
                    let mut states = states.lock().unwrap();
                    if let Some(existing) = states.iter_mut().find(|s| s.file_name == entry.file_name) {
                        let current_state = state.lock().unwrap().clone();
                        *existing = current_state;
                    } else {
                        states.push(state.lock().unwrap().clone());
                    }
                }

                drop(permit);
                pb.finish();
                overall_pb.inc(1);

                result
            });

            handles.push(handle);
        }

        // Wait for all downloads to complete
        let results: Vec<Result<()>> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| match r {
                Ok(res) => res,
                Err(e) => Err(anyhow!("Task error: {}", e)),
            })
            .collect();

        overall_pb.finish_with_message("Complete");

        // Save state
        let final_states = states.lock().unwrap().clone();
        self.save_state(&final_states)?;

        // Print summary
        let mut success = 0;
        let mut failed = 0;
        for result in &results {
            match result {
                Ok(_) => success += 1,
                Err(_) => failed += 1,
            }
        }
        println!("\nDone: {} success, {} failed", success, failed);

        Ok(())
    }

    async fn download_single(&self, entry: &LinkEntry) -> Result<()> {
        let state = Arc::new(Mutex::new(DownloadState::new(entry.file_name.clone())));

        let pb = ProgressBar::new(100);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} {bar:40} {bytes}/{total_bytes} ({bytes_per_sec})")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_message(entry.file_name.clone());

        self.download_file(entry, state, &pb).await?;

        Ok(())
    }
}

impl Clone for Downloader {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            output_dir: self.output_dir.clone(),
            state_file: self.state_file.clone(),
            resume: self.resume,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let link_file = args.link_file.unwrap_or_else(|| "sa-1b_link.txt".to_string());

    if !Path::new(&link_file).exists() {
        return Err(anyhow!("Link file not found: {}", link_file));
    }

    let downloader = Downloader::new(&args.output, args.resume)?;

    let mut entries = downloader.parse_link_file(&link_file)?;

    if entries.is_empty() {
        return Err(anyhow!("No entries found in link file"));
    }

    // Sort by file_name to ensure consistent ordering (sa_000000.tar, sa_000001.tar, etc.)
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
