use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DownloadMode {
    All,
    Single,
    Range,
}

#[derive(Parser, Debug)]
#[command(name = "sa-1b-dl")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Link file path (default: sa-1b_link.txt)
    #[arg(short, long)]
    pub link_file: Option<String>,

    /// Output directory
    #[arg(short, long, default_value = "./my_downloads")]
    pub output: String,

    /// Download mode
    #[arg(short, long, value_enum, default_value = "all")]
    pub mode: DownloadMode,

    /// Single file to download (file name from link file)
    #[arg(short = 'f', long)]
    pub file: Option<String>,

    /// Start index for range download (inclusive)
    #[arg(long, requires = "end")]
    pub start: Option<usize>,

    /// End index for range download (inclusive)
    #[arg(long, requires = "start")]
    pub end: Option<usize>,

    /// Number of parallel downloads
    #[arg(short, long, default_value = "4")]
    pub threads: usize,

    /// Resume interrupted downloads
    #[arg(long, default_value = "true")]
    pub resume: bool,

    /// HTTP proxy (e.g., http://127.0.0.1:7890)
    #[arg(long)]
    pub proxy: Option<String>,

    /// Number of retry attempts on failure
    #[arg(short, long, default_value = "3")]
    pub retries: u32,
}
