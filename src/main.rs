use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use ignore::{WalkBuilder, DirEntry};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output stamp file. Its contents and mtime will be equal to the max mtime.
    #[arg(long)]
    stamp: Option<PathBuf>,

    #[arg(long)]
    quiet: bool,

    /// Path to scan (defaults to current directory).
    path: Option<PathBuf>,
}

use std::time::{SystemTime, UNIX_EPOCH};

struct MtimeVisitor {
    // Max mtime for the thread.
    thread_max_mtime: SystemTime,
    // Max mtime for all threads.
    max_mtime: Arc<Mutex<SystemTime>>,
    // If any thread had an error.
    error: Arc<Mutex<Result<()>>>,
}

impl MtimeVisitor {
    fn new(max_mtime: Arc<Mutex<SystemTime>>, error: Arc<Mutex<Result<()>>>) -> Self {
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

impl MtimeVisitor {
    fn visit_inner(&mut self, entry: std::result::Result<ignore::DirEntry, ignore::Error>) -> Result<()> {
        let entry = entry.with_context(|| anyhow!("error reading directory entry"))?;
        let metadata = entry.metadata().with_context(|| anyhow!("error reading metadata for path {}", entry.path().display()))?;
        let mtime = metadata.modified().with_context(|| anyhow!("error getting modified time for path {}", entry.path().display()))?;
        self.thread_max_mtime = self.thread_max_mtime.max(mtime);
        Ok(())
    }
}

impl ignore::ParallelVisitor for MtimeVisitor {
    fn visit(
        &mut self,
        entry: std::result::Result<ignore::DirEntry, ignore::Error>,
    ) -> ignore::WalkState {

        match self.visit_inner(entry) {
            Ok(()) => ignore::WalkState::Continue,
            Err(e) => {
                *self.error.lock().unwrap() = Err(e);
                ignore::WalkState::Quit
            }
        }
    }
}

struct MtimeVisitorBuilder {
    // Max mtime overall.
    max_mtime: Arc<Mutex<SystemTime>>,
    // If any thread had an error.
    error: Arc<Mutex<Result<()>>>,
}

impl Default for MtimeVisitorBuilder {
    fn default() -> Self {
        Self {
            max_mtime: Arc::new(Mutex::new(UNIX_EPOCH)),
            error: Arc::new(Mutex::new(Ok(()))),
        }
    }
}

impl<'s> ignore::ParallelVisitorBuilder<'s> for MtimeVisitorBuilder {
    fn build(&mut self) -> Box<dyn ignore::ParallelVisitor + 's> {
        Box::new(MtimeVisitor::new(
            self.max_mtime.clone(),
            self.error.clone(),
        ))
    }
}

fn main() -> Result<()> {
    let cli: Cli = Cli::parse();

    let path = cli.path.as_deref().unwrap_or_else(|| Path::new("."));

    let mut visitor_builder = MtimeVisitorBuilder::default();

    WalkBuilder::new(path)
        .build_parallel()
        .visit(&mut visitor_builder);

    let max_mtime = *visitor_builder.max_mtime.lock().unwrap();
    let error = std::mem::replace(&mut *visitor_builder.error.lock().unwrap(), Ok(()));

    // Exit if any thread had an error.
    error?;

    let max_mtime_nanos = time::OffsetDateTime::from(max_mtime).unix_timestamp_nanos();

    if !cli.quiet {
        // Print the maximum mtime.
        println!("{}", max_mtime_nanos);
    }

    // If requested save it to a file and set that file's mtime to the
    // maximum mtime.
    if let Some(stamp) = &cli.stamp {
        std::fs::write(stamp, format!("{}\n", max_mtime_nanos))
            .with_context(|| anyhow!("error writing stamp file {}", stamp.display()))?;
        filetime::set_file_mtime(stamp, filetime::FileTime::from_system_time(max_mtime))
            .with_context(|| anyhow!("error setting mtime of stamp file {}", stamp.display()))?;
    }
    Ok(())
}

// Test module
#[cfg(test)]
mod tests {
    use super::*;

    use rand::distributions::Alphanumeric;
    use rand::Rng;

    fn rand_string(n: usize) -> String {
        let mut rng = rand::thread_rng();
        (0..n).map(|_| rng.sample(Alphanumeric) as char).collect()
    }

    fn set_mtime(path: &Path, max_mtime: &mut SystemTime) {
        let mut rng = rand::thread_rng();
        let mtime = UNIX_EPOCH + std::time::Duration::from_secs(rng.gen_range(0..100));
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(mtime)).unwrap();
        *max_mtime = mtime.max(*max_mtime);
    }

    #[test]
    fn test_mtime() {
        fn make_rand_dir(path: &Path, max_levels: usize, max_mtime: &mut SystemTime) {
            if max_levels == 0 {
                return;
            }
            // Create this directory.
            let dir = path.join(rand_string(10));
            std::fs::create_dir(&dir).unwrap();

            let mut rng = rand::thread_rng();

            // Create some directories.
            for _ in 0..rng.gen_range(0..3) {
                make_rand_dir(&dir, max_levels - 1, max_mtime);
            }
            // Create some files in this directory.
            for _ in 0..rng.gen_range(0..20) {
                let file_name = dir.join(rand_string(8));
                std::fs::write(&file_name, rand_string(100)).unwrap();
                // Set the mtime for this file.
                set_mtime(&file_name, max_mtime);
            }
            // Set the mtime for this directory.
            set_mtime(&dir, max_mtime);
        }

        // Create temporary directory.
        let temp_dir = tempfile::tempdir().unwrap();
        // let temp_dir_path = temp_dir.path();
        let temp_dir_path = temp_dir.path();

        // Create random directory structure with random mtimes.
        let root = temp_dir_path.join("root");
        std::fs::create_dir(&root).unwrap();

        // Max mtime for all files and directories.
        let mut max_mtime = UNIX_EPOCH;

        make_rand_dir(&root, 5, &mut max_mtime);

        set_mtime(&root, &mut max_mtime);

        let mut cmd = assert_cmd::Command::cargo_bin("maxtime").unwrap();

        cmd.arg(root);
        cmd.assert().success();
        cmd.assert().stdout(format!(
            "{}\n",
            time::OffsetDateTime::from(max_mtime).unix_timestamp_nanos()
        ));
    }
}
