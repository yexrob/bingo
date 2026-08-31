//! One process owns one app (ADR-0016 §5).
//!
//! Feishu's long connection is cluster-mode — a random client gets each event
//! — and Telegram's polling evicts whoever polled last. A second bingo on the
//! same credential is therefore not a conflict anybody sees: it is half the
//! messages going somewhere else, silently. So the claim is taken before the
//! adapter starts, and a claim that cannot be taken is a refusal with the
//! remedy in it, never a degraded start.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::ChannelError;

const DIRECTORY: &str = "channels";

/// A credential this process holds for as long as the value lives.
#[derive(Debug)]
pub struct Claim {
    path: PathBuf,
}

impl Claim {
    pub fn take(data_dir: &Path, adapter: &str, credential: &str) -> Result<Self, ChannelError> {
        let directory = data_dir.join(DIRECTORY);
        std::fs::create_dir_all(&directory)
            .map_err(|e| ChannelError::Refused(format!("{}: {e}", directory.display())))?;
        let path = directory.join(format!("{}.lock", slug(&format!("{adapter}-{credential}"))));
        let mut file = std::fs::File::create_new(&path).map_err(|_| taken(adapter, &path))?;
        // The pid is not read back — a lock file is proof of a claim, not of a
        // process — but it is what a person needs to check before removing it.
        let _ = write!(file, "{}", std::process::id());
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn taken(adapter: &str, path: &Path) -> ChannelError {
    ChannelError::Refused(format!(
        "another bingo already runs the {adapter} channel for this app: a second one \
         would take half of its events and neither would know. {} says so — remove it \
         if no bingo is running.",
        path.display()
    ))
}

/// A file name from an identifier that may hold anything: only what is safe
/// on every filesystem survives, and the rest becomes `_`.
fn slug(raw: &str) -> String {
    raw.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_claim_on_one_credential_is_refused_loudly() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let first = Claim::take(home.path(), "feishu", "cli_a1").expect("the first claim");
        let second = Claim::take(home.path(), "feishu", "cli_a1");
        let Err(ChannelError::Refused(message)) = second else {
            panic!("expected a refusal, got {second:?}");
        };
        assert!(message.contains("another bingo already runs"), "{message}");
        assert!(
            message.contains(&first.path().display().to_string()),
            "the remedy names the file: {message}"
        );
    }

    #[test]
    fn a_different_credential_is_a_different_claim() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let _a = Claim::take(home.path(), "feishu", "cli_a1").expect("a");
        let _b = Claim::take(home.path(), "feishu", "cli_b2").expect("b");
        let _c = Claim::take(home.path(), "loopback", "cli_a1").expect("c");
    }

    #[test]
    fn a_claim_is_given_back_when_it_is_dropped() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let path = {
            let claim = Claim::take(home.path(), "feishu", "cli_a1").expect("a claim");
            claim.path().to_path_buf()
        };
        assert!(!path.exists());
        Claim::take(home.path(), "feishu", "cli_a1").expect("the next process may have it");
    }

    #[test]
    fn a_credential_with_a_path_in_it_cannot_escape_the_directory() {
        let home = tempfile::tempdir().expect("a temporary directory");
        let claim = Claim::take(home.path(), "loopback", "../../etc/passwd").expect("a claim");
        assert_eq!(
            claim.path().parent(),
            Some(home.path().join(DIRECTORY).as_path())
        );
    }
}
