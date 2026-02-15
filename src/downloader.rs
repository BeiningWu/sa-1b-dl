use crate::models::{DownloadState, LinkEntry};
use crate::state::StateManager;
use anyhow::{anyhow, Context, Result};
use futures::future::join_all;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest::Client;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task;

pub struct Downloader {
    client: Client,
    output_dir: PathBuf,
    state_manager: StateManager,
    resume: bool,
    retries: u32,
}

impl Downloader {
    pub fn new(output_dir: &str, resume: bool, proxy: Option<&str>, retries: u32) -> Result<Self> {
        let output_path = PathBuf::from(output_dir);
        if !output_path.exists() {
            fs::create_dir_all(&output_path)?;
        }

        let state_manager = StateManager::new(&output_path);

        let mut client_builder = Client::builder()
            .timeout(Duration::from_secs(300));

        if let Some(proxy_url) = proxy {
            client_builder = client_builder.proxy(reqwest::Proxy::http(proxy_url)?);
        }

        let client = client_builder
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            client,
            output_dir: output_path,
            state_manager,
            resume,
            retries,
        })
    }

    pub fn parse_link_file(&self, path: &str) -> Result<Vec<LinkEntry>> {
        let file = File::open(path).context("Failed to open link file")?;
        let reader = BufReader::new(file);

        let mut entries = Vec::new();

        for (idx, line) in reader.lines().enumerate() {
            let line = line.context("Failed to read line")?;

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

    async fn download_file_with_retry(
        &self,
        entry: &LinkEntry,
        state: Arc<Mutex<DownloadState>>,
        pb: &ProgressBar,
    ) -> Result<()> {
        let mut attempt = 0u32;
        let _partial_path = format!("{}.part", self.output_dir.join(&entry.file_name).display());
        let original_message = pb.message().to_string();

        loop {
            attempt += 1;

            match self.download_file(entry, state.clone(), pb).await {
                Ok(_) => {
                    // 成功后恢复原来的消息（移除重试信息）
                    if attempt > 1 && !pb.is_finished() {
                        pb.set_message(original_message.clone());
                    }
                    return Ok(());
                }
                Err(e) if attempt < self.retries => {
                    // Wait before retry (exponential backoff: 1s, 2s, 4s...)
                    let delay_ms = 1000 * (1 << (attempt - 1)).min(30000);

                    // 在进度条上显示重试信息
                    pb.set_message(format!("{} [Retry {}/{}: {}s wait...]", original_message, attempt, self.retries, delay_ms / 1000));
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;

                    // 恢复进度条显示（如果还没完成）
                    if !pb.is_finished() {
                        pb.set_message(format!("{} [Retry {}/{}: {}]", original_message, attempt, self.retries, e));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn download_file(
        &self,
        entry: &LinkEntry,
        state: Arc<Mutex<DownloadState>>,
        pb: &ProgressBar,
    ) -> Result<()> {
        let output_path = self.output_dir.join(&entry.file_name);
        let partial_path = format!("{}.part", output_path.display());

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

        if output_path.exists() {
            let actual_size = fs::metadata(&output_path)?.len();
            let is_valid = if let Some(expected) = total_bytes {
                actual_size == expected
            } else {
                actual_size > 0
            };

            if is_valid {
                {
                    let mut state = state.lock().unwrap();
                    state.completed = true;
                    state.downloaded_bytes = actual_size;
                }
                pb.set_message("Skipped (valid)");
                pb.finish();
                return Ok(());
            } else {
                fs::remove_file(&output_path)?;
                pb.set_message("Removed invalid file, re-downloading...");
            }
        }

        let mut current_pos = 0u64;
        if self.resume && Path::new(&partial_path).exists() {
            current_pos = fs::metadata(&partial_path)?.len();
        }

        if let Some(total) = total_bytes {
            if current_pos > 0 && current_pos > total {
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
                self.rename_partial_to_complete(&partial_path, &output_path)?;
                self.finalize_download(entry, state, pb, total_bytes, true)?;
                return Ok(());
            }

            pb.set_length(total);
            pb.set_position(current_pos);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{msg:30} {bar:40} {bytes}/{total_bytes} ({bytes_per_sec})")
                    .unwrap()
                    .progress_chars("=>-"),
            );
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&partial_path)
            .context("Failed to open output file")?;

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

        fs::rename(&partial_path, &output_path)?;

        let actual_size = fs::metadata(&output_path)?.len();
        let expected_size = total_bytes.unwrap_or(0);

        let is_valid = if expected_size > 0 {
            actual_size == expected_size
        } else {
            actual_size > 1024
        };

        self.finalize_download(entry, state, pb, total_bytes, is_valid)?;

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

    fn rename_partial_to_complete(&self, partial_path: &str, output_path: &PathBuf) -> Result<()> {
        fs::rename(partial_path, output_path).context("Failed to rename completed file")?;
        Ok(())
    }

    fn finalize_download(
        &self,
        entry: &LinkEntry,
        state: Arc<Mutex<DownloadState>>,
        pb: &ProgressBar,
        _total_bytes: Option<u64>,
        is_valid: bool,
    ) -> Result<()> {
        let actual_size = fs::metadata(&self.output_dir.join(&entry.file_name))?.len();

        {
            let mut state = state.lock().unwrap();
            state.completed = is_valid;
            state.downloaded_bytes = actual_size;
        }

        if is_valid {
            pb.set_message("Done");
        } else {
            pb.set_message("Size mismatch!");
        }
        pb.finish();

        Ok(())
    }

    pub async fn download_all(&self, entries: Vec<LinkEntry>, num_threads: usize) -> Result<()> {
        let states: Arc<Mutex<Vec<DownloadState>>> =
            Arc::new(Mutex::new(self.state_manager.load_state()?));
        let mp = Arc::new(MultiProgress::new());

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
            let downloader = self.clone();

            let permit = semaphore.clone().acquire_owned().await.unwrap();

            let handle = task::spawn(async move {
                let pb = mp.add(ProgressBar::new(100));
                let msg = format!("[{:>2}] {}", idx, entry.file_name);
                pb.set_message(msg);
                pb.set_style(
                    ProgressStyle::default_bar()
                        .template("{msg}")
                        .unwrap()
                        .progress_chars("=>-"),
                );

                let result =
                    downloader.download_file_with_retry(&entry, Arc::clone(&state), &pb).await;

                {
                    let mut states = states.lock().unwrap();
                    if result.is_ok() {
                        if let Some(existing) = states.iter_mut().find(|s| s.file_name == entry.file_name)
                        {
                            let current_state = state.lock().unwrap().clone();
                            *existing = current_state;
                        } else {
                            states.push(state.lock().unwrap().clone());
                        }
                    }
                }

                drop(permit);
                pb.finish();

                result
            });

            handles.push(handle);
        }

        let results: Vec<Result<()>> = join_all(handles)
            .await
            .into_iter()
            .map(|r| match r {
                Ok(res) => res,
                Err(e) => Err(anyhow!("Task error: {}", e)),
            })
            .collect();

        // 清除文件进度条
        mp.clear().ok();

        let final_states = states.lock().unwrap().clone();
        self.state_manager.save_state(&final_states)?;

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

    pub async fn download_single(&self, entry: &LinkEntry) -> Result<()> {
        let state = Arc::new(Mutex::new(DownloadState::new(entry.file_name.clone())));

        let pb = ProgressBar::new(100);
        pb.set_style(
            ProgressStyle::default_bar()
                .template("{msg} {bar:40} {bytes}/{total_bytes} ({bytes_per_sec})")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb.set_message(entry.file_name.clone());

        self.download_file_with_retry(entry, state, &pb).await?;

        Ok(())
    }
}

impl Clone for Downloader {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            output_dir: self.output_dir.clone(),
            state_manager: StateManager::new(&self.output_dir),
            resume: self.resume,
            retries: self.retries,
        }
    }
}
