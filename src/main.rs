use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64};

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

use std::time::UNIX_EPOCH;

pub fn mtime(path: &Path) -> Result<u64> {
    // 64 bits of nanoseconds gives 500 years so we should be fine still.
    let nanos = std::fs::metadata(path)
        .with_context(|| anyhow!("error reading file metadata for {}", path.display()))?
        .modified()
        .with_context(|| anyhow!("error reading mtime for {}", path.display()))?
        .duration_since(UNIX_EPOCH)
        .with_context(|| anyhow!("file modified before UNIX Epoch {}", path.display()))?
        .as_nanos();

    u64::try_from(nanos).with_context(|| anyhow!("mtime too far in future {nanos}"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let path = cli.path.as_deref().unwrap_or_else(|| Path::new("."));

    let mut max_mtime = AtomicU64::new(0);
    let mut error = AtomicBool::new(false);

    let visitor =
        |path: std::result::Result<ignore::DirEntry, ignore::Error>| -> ignore::WalkState {
            match path {
                Ok(path) => match mtime(path.path()) {
                    Ok(mtime) => {
                        max_mtime.fetch_max(mtime, std::sync::atomic::Ordering::SeqCst);
                        ignore::WalkState::Continue
                    }
                    Err(e) => {
                        eprintln!(
                            "Error getting mtime for path {}: {e}",
                            path.path().display()
                        );
                        error.store(true, std::sync::atomic::Ordering::SeqCst);
                        ignore::WalkState::Quit
                    }
                },
                Err(e) => {
                    eprintln!("Error: {e}");
                    error.store(true, std::sync::atomic::Ordering::SeqCst);
                    ignore::WalkState::Quit
                }
            }
        };

    WalkBuilder::new(path)
        .build_parallel()
        .run(|| Box::new(visitor));

    let error = *error.get_mut();
    let max_mtime = *max_mtime.get_mut();

    if error {
        // Error already printed above.
        bail!("");
    }

    // Print the maximum mtime.
    println!("{}", max_mtime);

    // If requested save it to a file and set that file's mtime to the
    // maximum mtime.
    if let Some(stamp) = &cli.stamp {
        std::fs::write(stamp, format!("{}\n", max_mtime))
            .with_context(|| anyhow!("error writing stamp file {}", stamp.display()))?;
        filetime::set_file_mtime(
            stamp,
            filetime::FileTime::from_unix_time(
                (max_mtime / 1_000_000_000) as i64,
                (max_mtime % 1_000_000_000) as u32,
            ),
        )
        .with_context(|| anyhow!("error setting mtime of stamp file {}", stamp.display()))?;
    }
    Ok(())
}
