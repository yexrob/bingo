//! Publish a complete marker without exposing a blank legacy-shaped file.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::MARKER;

pub(super) fn open(path: &Path) -> io::Result<File> {
    match existing(path) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            publish(path)?;
            existing(path)
        }
        Err(e) => Err(e),
    }
}

fn existing(path: &Path) -> io::Result<File> {
    File::options().read(true).write(true).open(path)
}

fn publish(path: &Path) -> io::Result<()> {
    let staging = path.with_file_name(format!(".runner-{}.tmp", ulid::Ulid::generate()));
    let mut file = File::create_new(&staging)?;
    let _cleanup = Staging(staging.clone());
    file.write_all(MARKER.as_bytes())?;
    file.sync_all()?;
    drop(file);
    // A hard link publishes complete bytes atomically without replacing an
    // inode a modern runner has locked or a legacy runner has claimed.
    match std::fs::hard_link(staging, path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
