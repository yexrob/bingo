//! Becoming the newest release: unpack the archive, and put what is in it
//! where this binary is.
//!
//! Nothing here is run with elevated rights, ever. A directory this process
//! may not write is a failure with the way round in it, not a prompt for a
//! password.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("cannot tell where this binary is: {0}")]
    Running(#[source] std::io::Error),
    #[error("cannot run tar: {0}")]
    Tar(#[source] std::io::Error),
    #[error("cannot unpack {archive}: {message}")]
    Unpack { archive: String, message: String },
    #[error("the archive holds no {0}")]
    NoBinary(String),
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl InstallError {
    /// Whether this is a directory the person running is not allowed to
    /// write — the one failure with an answer of its own.
    pub fn is_permission(&self) -> bool {
        let source = match self {
            InstallError::Write { source, .. } | InstallError::Running(source) => source,
            _ => return false,
        };
        source.kind() == std::io::ErrorKind::PermissionDenied
    }
}

/// The file this process is running from, with every symlink resolved: a
/// Homebrew or cargo shim points at the binary, and it is the binary that has
/// to be replaced, not the link to it.
pub fn running() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe().map_err(InstallError::Running)?;
    exe.canonicalize().map_err(InstallError::Running)
}

/// Where a run keeps the archive while it is reading it. Its own, per
/// process, so two runs cannot unpack into each other.
pub fn staging(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("update.{}", std::process::id()))
}

/// Unpack with the system tar and answer with the binary inside it.
///
/// `tar` is the one archiver every platform the release ships already has:
/// Windows has carried `tar.exe` (bsdtar, which reads zip as well as gzip)
/// since 10 1803, so no crate is bought for this.
pub fn unpack(archive: &Path, into: &Path) -> Result<PathBuf, InstallError> {
    made(into)?;
    let out = Command::new("tar")
        .arg(flags(archive))
        .arg(archive)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(InstallError::Tar)?;
    if !out.status.success() {
        return Err(InstallError::Unpack {
            archive: archive.display().to_string(),
            message: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    let binary = into.join(crate::asset::binary());
    match binary.is_file() {
        true => Ok(binary),
        false => Err(InstallError::NoBinary(crate::asset::binary())),
    }
}

/// The flags this archive needs: a zip is not a gzip, and Windows' release is
/// the one that is a zip.
fn flags(archive: &Path) -> &'static str {
    let zip = archive
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
    match zip {
        true => "-xf",
        false => "-xzf",
    }
}

/// A copy of the new binary beside the one it replaces. A rename may not
/// cross a filesystem, and the directory a download landed in often is one.
pub fn stage(new: &Path, target: &Path) -> Result<PathBuf, InstallError> {
    let staged = target.with_extension("new");
    let failed = |source| InstallError::Write {
        path: staged.display().to_string(),
        source,
    };
    let _ = std::fs::remove_file(&staged);
    std::fs::copy(new, &staged).map_err(failed)?;
    executable(&staged).map_err(failed)?;
    Ok(staged)
}

/// Put `staged` where `target` is, by the two renames Windows needs.
///
/// A running binary may be *renamed* on both unix and Windows, but only unix
/// lets one be overwritten — so the running file is moved aside first and the
/// new one takes its place. Removing what was moved aside is best effort:
/// Windows holds the running image open, and what is left is swept by the
/// next start ([`sweep`]).
pub fn swap(staged: &Path, target: &Path) -> Result<(), InstallError> {
    let old = retired(target);
    let _ = std::fs::remove_file(&old);
    let failed = |path: &Path| {
        let path = path.display().to_string();
        move |source| InstallError::Write { path, source }
    };
    std::fs::rename(target, &old).map_err(failed(target))?;
    if let Err(source) = std::fs::rename(staged, target) {
        // Put the running binary back: a failed update leaves nothing behind.
        let _ = std::fs::rename(&old, target);
        let _ = std::fs::remove_file(staged);
        return Err(failed(target)(source));
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// The binary a previous update moved aside and could not remove. Called at
/// the start of a run, best effort: a file that is still held stays until the
/// start after this one.
pub fn sweep(target: &Path) {
    let _ = std::fs::remove_file(retired(target));
}

/// Where the running binary is moved to while the new one takes its place.
fn retired(target: &Path) -> PathBuf {
    target.with_extension("old")
}

fn made(dir: &Path) -> Result<(), InstallError> {
    std::fs::create_dir_all(dir).map_err(|source| InstallError::Write {
        path: dir.display().to_string(),
        source,
    })
}

/// A binary is only a binary if it may be run. `fs::copy` carries the mode
/// across on unix, but an archive packed without one would leave a file
/// nobody can start; Windows has no mode to set.
#[cfg(unix)]
fn executable(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn executable(_path: &Path) -> Result<(), std::io::Error> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `tar.gz` holding one file called `bingo`, built by the same tar that
    /// will unpack it.
    fn archived(dir: &Path, contents: &str) -> PathBuf {
        let inside = dir.join("packed");
        std::fs::create_dir_all(&inside).expect("a directory");
        std::fs::write(inside.join(crate::asset::binary()), contents).expect("the binary");
        let archive = dir.join("bingo.tar.gz");
        let out = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&inside)
            .arg(crate::asset::binary())
            .output()
            .expect("tar runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        archive
    }

    #[test]
    fn a_zip_is_not_unpacked_as_a_gzip() {
        assert_eq!(flags(Path::new("bingo-x86_64-pc-windows-msvc.zip")), "-xf");
        assert_eq!(flags(Path::new("bingo-x86_64-apple-darwin.tar.gz")), "-xzf");
        assert_eq!(flags(Path::new("bingo.TAR.GZ")), "-xzf");
        assert_eq!(flags(Path::new("bingo.ZIP")), "-xf");
    }

    #[test]
    fn the_archive_gives_up_the_binary_it_holds() {
        let dir = tempfile::tempdir().expect("a directory");
        let archive = archived(dir.path(), "the new bingo");
        let unpacked = unpack(&archive, &dir.path().join("out")).expect("it unpacks");
        assert_eq!(
            std::fs::read_to_string(&unpacked).expect("the binary"),
            "the new bingo"
        );
    }

    #[test]
    fn an_archive_that_is_not_one_fails_and_says_what_tar_said() {
        let dir = tempfile::tempdir().expect("a directory");
        let archive = dir.path().join("bingo.tar.gz");
        std::fs::write(&archive, b"not an archive").expect("a file");
        let error = unpack(&archive, &dir.path().join("out")).expect_err("tar refuses it");
        assert!(matches!(error, InstallError::Unpack { .. }), "{error}");
    }

    #[test]
    fn the_two_renames_replace_the_target_and_leave_nothing_beside_it() {
        let dir = tempfile::tempdir().expect("a directory");
        let target = dir.path().join("bingo");
        std::fs::write(&target, "the old bingo").expect("the target");
        let staged = stage(&dir.path().join("new"), &target).err();
        assert!(staged.is_some(), "there is nothing to stage yet");

        std::fs::write(dir.path().join("new"), "the new bingo").expect("the new one");
        let staged = stage(&dir.path().join("new"), &target).expect("it stages");
        swap(&staged, &target).expect("it swaps");

        assert_eq!(
            std::fs::read_to_string(&target).expect("the target"),
            "the new bingo"
        );
        assert!(!retired(&target).exists(), "the old one is gone");
        assert!(!staged.exists(), "and so is the staged one");
    }

    #[cfg(unix)]
    #[test]
    fn what_is_staged_may_be_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a directory");
        std::fs::write(dir.path().join("new"), "#!/bin/sh\n").expect("the new one");
        let staged = stage(&dir.path().join("new"), &dir.path().join("bingo")).expect("it stages");
        let mode = std::fs::metadata(&staged)
            .expect("it is there")
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "{mode:o}");
    }

    #[test]
    fn a_binary_left_behind_by_an_earlier_update_is_swept() {
        let dir = tempfile::tempdir().expect("a directory");
        let target = dir.path().join("bingo");
        std::fs::write(retired(&target), "the one before").expect("a leftover");
        sweep(&target);
        assert!(!retired(&target).exists());
        sweep(&target);
    }

    #[test]
    fn a_target_that_is_not_there_fails_and_leaves_the_staged_one_alone() {
        let dir = tempfile::tempdir().expect("a directory");
        let staged = dir.path().join("bingo.new");
        std::fs::write(&staged, "the new bingo").expect("the staged one");
        let error = swap(&staged, &dir.path().join("bingo")).expect_err("there is nothing there");
        assert!(matches!(error, InstallError::Write { .. }), "{error}");
    }

    #[test]
    fn a_directory_nobody_may_write_says_so_and_names_the_way_round() {
        let error = InstallError::Write {
            path: "/usr/local/bin/bingo".into(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert!(error.is_permission());
        assert!(!InstallError::NoBinary("bingo".into()).is_permission());
    }
}
