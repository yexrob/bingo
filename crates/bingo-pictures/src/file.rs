//! A picture as a file on this machine.
//!
//! Two things every write here has in common, spelled once: the file appears
//! whole or not at all — a temporary name beside it and a rename, so a second
//! bingo sharing the directory never reads half a picture — and a name that
//! is a hash, so the same bytes are one file however often they are handed
//! over and the name is a name on every file system.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bingo_sdk::Image;

/// Write `bytes` at `path`, the directory made if it is not there, through
/// a temporary name and a rename. On a rename that fails the temporary is
/// removed, so a directory that will not take the file is left as found.
pub fn written(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = temporary(path);
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

/// The picture written under `dir` as `<hash>.<ext>`, and where that is. The
/// name is the bytes' own, so pasting the same picture twice leaves one file;
/// the path comes back absolute, because it is going into a transcript whose
/// reader's directory is not this surface's business (ADR-0051, ADR-0052).
pub fn keep(dir: &Path, image: &Image) -> std::io::Result<PathBuf> {
    let extension = Image::extension_of(&image.media_type).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a picture type")
    })?;
    let bytes = decoded(image)?;
    let path = dir.join(format!("{}.{extension}", named(&bytes)));
    written(&path, &bytes)?;
    Ok(std::path::absolute(&path).unwrap_or(path))
}

fn decoded(image: &Image) -> std::io::Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// The name one write uses before its rename: the file's own name, this
/// process, and a number no other write in it repeats — two processes, or two
/// tests, must not rename each other's half-written file into place.
fn temporary(path: &Path) -> PathBuf {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().map(|name| name.to_string_lossy());
    path.with_file_name(format!(
        ".{}.{}.{n}.tmp",
        name.unwrap_or_default(),
        std::process::id()
    ))
}

/// The name for some bytes: their hash in hex, so the name is short, is a
/// name on every file system, and is the same on every run.
pub fn named(bytes: &[u8]) -> String {
    format!("{:032x}", hashed(bytes))
}

const FNV_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

/// FNV-1a over 128 bits, spelled here because no digest of that width is in
/// this workspace's dependency tree and one crate over the budget is one too
/// many (`scripts/budget.toml`). It has one job — telling two pictures, or
/// two addresses, apart — and 128 bits is far more than a directory of a few
/// hundred entries can collide in. It is not a signature and nothing here
/// treats it as one.
fn hashed(bytes: &[u8]) -> u128 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u128::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_picture_is_named_by_its_bytes_and_read_back_whole() {
        let dir = tempfile::tempdir().unwrap();
        let image = Image::from_bytes("image/png", b"pixels").unwrap();
        let path = keep(dir.path(), &image).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"pixels");
        assert_eq!(path.extension().and_then(|e| e.to_str()), Some("png"));
        assert!(path.is_absolute());
        assert_eq!(
            keep(dir.path(), &image).unwrap(),
            path,
            "the same bytes are one file"
        );
        let other = Image::from_bytes("image/jpeg", b"other").unwrap();
        assert_ne!(keep(dir.path(), &other).unwrap(), path);
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 2, "no temporary is left behind");
    }

    #[test]
    fn a_write_makes_its_directory_and_leaves_nothing_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep").join("er").join("a.bin");
        written(&path, b"x").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x");
        // A directory standing where the file should go: the rename fails and
        // the temporary goes with it.
        let blocked = dir.path().join("blocked");
        std::fs::create_dir_all(blocked.join("sub")).unwrap();
        assert!(written(&blocked, b"y").is_err());
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }
}
