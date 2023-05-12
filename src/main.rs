use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use ignore::WalkBuilder;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to scan (defaults to current directory).
    path: Option<PathBuf>,

    /// Output stamp file. Its contents and mtime will be equal to the max mtime.
    #[arg(long)]
    stamp: Option<PathBuf>,
}

use std::time::{SystemTime, UNIX_EPOCH};

pub fn mtime(path: &Path) -> std::io::Result<SystemTime> {
    std::fs::metadata(path)?.modified()
}

struct MtimeVisitor {
    // Max mtime for the thread.
    thread_max_mtime: SystemTime,
    // Max mtime for all threads.
    max_mtime: Arc<Mutex<SystemTime>>,
    // If any thread had an error.
    error: Arc<AtomicBool>,
}

impl MtimeVisitor {
    fn new(max_mtime: Arc<Mutex<SystemTime>>, error: Arc<AtomicBool>) -> Self {
        Self {
            thread_max_mtime: UNIX_EPOCH,
            max_mtime,
            error,
        }
    }
}

impl Drop for MtimeVisitor {
    fn drop(&mut self) {
        let mut max_mtime = self.max_mtime.lock().unwrap();
        *max_mtime = max_mtime.max(self.thread_max_mtime);
    }
}

impl ignore::ParallelVisitor for MtimeVisitor {
    fn visit(&mut self, entry: std::result::Result<ignore::DirEntry, ignore::Error>) -> ignore::WalkState {
        match entry {
            Ok(entry) => match mtime(entry.path()) {
                Ok(mtime) => {
                    self.thread_max_mtime = self.thread_max_mtime.max(mtime);
                    ignore::WalkState::Continue
                }
                Err(e) => {
                    eprintln!(
                        "Error getting mtime for path {}: {e}",
                        entry.path().display()
                    );
                    self.error.store(true, std::sync::atomic::Ordering::SeqCst);
                    ignore::WalkState::Quit
                }
            },
            Err(e) => {
                eprintln!("Error: {e}");
                self.error.store(true, std::sync::atomic::Ordering::SeqCst);
                ignore::WalkState::Quit
            }
        }
    }
}

struct MtimeVisitorBuilder {
    // Max mtime overall.
    max_mtime: Arc<Mutex<SystemTime>>,
    // If any thread had an error.
    error: Arc<AtomicBool>,
}

impl Default for MtimeVisitorBuilder {
    fn default() -> Self {
        Self { max_mtime: Arc::new(Mutex::new(UNIX_EPOCH)), error: Arc::new(AtomicBool::new(false)) }
    }
}

impl<'s> ignore::ParallelVisitorBuilder<'s> for MtimeVisitorBuilder
{
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 's> {
        Box::new(MtimeVisitor::new(self.max_mtime.clone(), self.error.clone()))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let path = cli.path.as_deref().unwrap_or_else(|| Path::new("."));

    let mut visitor_builder = MtimeVisitorBuilder::default();

    WalkBuilder::new(path)
        .build_parallel()
        .visit(&mut visitor_builder);

    let error = visitor_builder.error.load(std::sync::atomic::Ordering::SeqCst);
    let max_mtime = visitor_builder.max_mtime.lock().unwrap().clone();

    if error {
        // Error already printed above.
        bail!("");
    }

    let max_mtime_nanos = time::OffsetDateTime::from(max_mtime).unix_timestamp_nanos();

    // Print the maximum mtime.
    println!("{}", max_mtime_nanos);

    // If requested save it to a file and set that file's mtime to the
    // maximum mtime.
    if let Some(stamp) = &cli.stamp {
        std::fs::write(stamp, format!("{}\n", max_mtime_nanos))
            .with_context(|| anyhow!("error writing stamp file {}", stamp.display()))?;
        filetime::set_file_mtime(
            stamp,
            filetime::FileTime::from_system_time(max_mtime),
        )
        .with_context(|| anyhow!("error setting mtime of stamp file {}", stamp.display()))?;
    }
    Ok(())
}
