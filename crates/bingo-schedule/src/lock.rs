//! One runner per store (ADR-0019 §5).
//!
//! Two bingo processes over one store would each fire every schedule, and
//! nobody would see two turns and know why. So the timer loop starts only
//! behind this claim, and a process that cannot take it runs with schedules
//! dormant and says who has them — the channels plugin's credential lock,
//! for the same reason and in the same shape.
//!
//! A claim is proof that a process took the store, not proof that it still
//! runs: a bingo that was killed leaves its file behind, and the line below
//! is what tells a person to remove it.

use std::io::Write;
use std::path::{Path, PathBuf};

const LOCK: &str = "runner.lock";

/// The store's runner, held for as long as the value lives.
#[derive(Debug)]
pub struct Claim {
    path: PathBuf,
}

impl Claim {
    /// Take the runner's claim, or say who holds it.
    pub fn take(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(LOCK);
        let mut file = std::fs::File::create_new(&path).map_err(|_| holder(dir, false))?;
        // The pid is not read back to decide anything — a lock file is proof
        // of a claim, not of a process — but it is what a person needs to
        // check before removing it.
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

/// The one line that says whether schedules fire here, written once and read
/// by `/schedule`, by every tool's receipt and by the runner's own notice.
pub fn holder(dir: &Path, held: bool) -> String {
    if held {
        return "held by this process".into();
    }
    let path = dir.join(LOCK);
    match std::fs::read_to_string(&path) {
        Ok(pid) => format!(
            "dormant — held by pid {} ({}); remove it if no bingo is running",
            pid.trim(),
            path.display()
        ),
        Err(_) => "dormant — no runner holds this store".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    #[test]
    fn the_second_claim_on_one_store_is_refused_with_the_pid_that_has_it() {
        let home = dir();
        let first = Claim::take(home.path()).expect("the first claim");
        let refusal = Claim::take(home.path()).expect_err("the second is dormant");
        assert!(
            refusal.contains(&format!("pid {}", std::process::id())),
            "{refusal}"
        );
        assert!(
            refusal.contains(&first.path().display().to_string()),
            "{refusal}"
        );
        assert!(
            refusal.contains("remove it if no bingo is running"),
            "{refusal}"
        );
    }

    #[test]
    fn a_claim_is_given_back_when_it_is_dropped() {
        let home = dir();
        let path = {
            let claim = Claim::take(home.path()).expect("a claim");
            claim.path().to_path_buf()
        };
        assert!(!path.exists());
        Claim::take(home.path()).expect("the next process may have it");
    }

    #[test]
    fn the_claim_lands_beside_the_entries_and_is_not_one() {
        let home = dir();
        let claim = Claim::take(home.path()).expect("a claim");
        assert_eq!(claim.path().parent(), Some(home.path()));
        assert_eq!(claim.path().file_name().expect("a name"), "runner.lock");
    }

    #[test]
    fn the_holder_line_says_this_process_a_pid_or_nobody() {
        let home = dir();
        assert_eq!(holder(home.path(), true), "held by this process");
        assert_eq!(
            holder(home.path(), false),
            "dormant — no runner holds this store"
        );
        let _claim = Claim::take(home.path()).expect("a claim");
        assert!(holder(home.path(), false).starts_with("dormant — held by pid "));
        assert_eq!(
            holder(home.path(), true),
            "held by this process",
            "a process that holds it does not read the file to find out"
        );
    }
}
