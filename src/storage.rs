use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::error::ErrorCode;

pub const RETENTION_TTL_DAYS: u64 = 30;
pub const RETENTION_MAX_SESSIONS: usize = 100;
pub const RETENTION_MAX_HISTORY_FILES: usize = 100;
const RECENT_ACTIVITY_GRACE_SECS: u64 = 24 * 60 * 60;

static CLEANUP_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("cannot determine the user home directory; set HOME (or USERPROFILE on Windows)")]
    HomeUnavailable,
    #[error("failed to {operation} {path}: {source}", path = .path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl ErrorCode for StorageError {
    fn error_code(&self) -> &'static str {
        match self {
            StorageError::HomeUnavailable => "CONFIG_INVALID",
            StorageError::Io { .. } => "STORAGE_ERROR",
        }
    }
}

pub fn resolve_home() -> Result<PathBuf, StorageError> {
    resolve_home_from(
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
        cfg!(windows),
    )
}

fn resolve_home_from(
    home: Option<OsString>,
    user_profile: Option<OsString>,
    windows: bool,
) -> Result<PathBuf, StorageError> {
    let absolute = |value: Option<OsString>| {
        value
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    absolute(home)
        .or_else(|| windows.then(|| absolute(user_profile)).flatten())
        .ok_or(StorageError::HomeUnavailable)
}

pub fn data_dir(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("bingo")
}

pub fn transcripts_dir(home: &Path) -> PathBuf {
    data_dir(home).join("transcripts")
}

pub fn history_dir(home: &Path) -> PathBuf {
    data_dir(home).join("history")
}

pub fn shares_dir(home: &Path) -> PathBuf {
    data_dir(home).join("shares")
}

pub fn tasks_dir(home: &Path) -> PathBuf {
    data_dir(home).join("tasks")
}

#[derive(Debug, Clone, Copy)]
struct RetentionPolicy {
    ttl: Duration,
    max_sessions: usize,
    max_history_files: usize,
    recent_activity_grace: Duration,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(RETENTION_TTL_DAYS * 24 * 60 * 60),
            max_sessions: RETENTION_MAX_SESSIONS,
            max_history_files: RETENTION_MAX_HISTORY_FILES,
            recent_activity_grace: Duration::from_secs(RECENT_ACTIVITY_GRACE_SECS),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CleanupReport {
    pub transcripts: usize,
    pub shares: usize,
    pub history_files: usize,
}

impl CleanupReport {
    pub fn total(&self) -> usize {
        self.transcripts + self.shares + self.history_files
    }

    pub fn summary(&self) -> String {
        if self.total() == 0 {
            return format!(
                "no expired session data (TTL {RETENTION_TTL_DAYS} days; latest {RETENTION_MAX_SESSIONS} inactive sessions kept; 24-hour activity grace)"
            );
        }
        format!(
            "cleaned {} transcript(s), {} share file(s), and {} history file(s) (TTL {RETENTION_TTL_DAYS} days; latest {RETENTION_MAX_SESSIONS} inactive sessions kept; 24-hour activity grace)",
            self.transcripts, self.shares, self.history_files
        )
    }
}

#[derive(Debug, Clone)]
struct StoredEntry {
    path: PathBuf,
    modified: SystemTime,
}

pub fn cleanup(
    home: &Path,
    protected_transcript: Option<&Path>,
) -> Result<CleanupReport, StorageError> {
    cleanup_with_policy_at(
        home,
        protected_transcript,
        RetentionPolicy::default(),
        SystemTime::now(),
    )
}

fn cleanup_with_policy_at(
    home: &Path,
    protected_transcript: Option<&Path>,
    policy: RetentionPolicy,
    now: SystemTime,
) -> Result<CleanupReport, StorageError> {
    let _guard = CLEANUP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut report = CleanupReport::default();

    let transcript_entries = collect_files(&transcripts_dir(home))?
        .into_iter()
        .filter(|entry| entry.path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    let transcript_removals = select_removals(
        &transcript_entries,
        now,
        policy.ttl,
        policy.max_sessions,
        policy.recent_activity_grace,
        protected_transcript,
    );
    for path in transcript_removals {
        let session_key = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string());
        if !remove_inactive_transcript(&path)? {
            continue;
        }
        report.transcripts += 1;
        if let Some(key) = session_key {
            report.shares += remove_share_files(&shares_dir(home), &key)?;
        }
    }

    let history_entries = collect_files(&history_dir(home))?
        .into_iter()
        .filter(|entry| entry.path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    for entry in select_removal_entries(
        &history_entries,
        now,
        policy.ttl,
        policy.max_history_files,
        Duration::ZERO,
        None,
    ) {
        if remove_unchanged_file(&entry)? {
            report.history_files += 1;
        }
    }

    Ok(report)
}

fn remove_share_files(dir: &Path, session_key: &str) -> Result<usize, StorageError> {
    let mut removed = 0;
    for suffix in ["json", "json.bak", "json.tmp"] {
        removed += usize::from(remove_file(&dir.join(format!("{session_key}.{suffix}")))?);
    }
    Ok(removed)
}

fn collect_files(dir: &Path) -> Result<Vec<StoredEntry>, StorageError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error("read directory", dir, source)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read directory entry in", dir, source))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| io_error("read file type for", &path, source))?;
        if !file_type.is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|source| io_error("read metadata for", &path, source))?;
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };
        files.push(StoredEntry { path, modified });
    }
    Ok(files)
}

fn select_removals(
    entries: &[StoredEntry],
    now: SystemTime,
    ttl: Duration,
    limit: usize,
    recent_activity_grace: Duration,
    protected: Option<&Path>,
) -> Vec<PathBuf> {
    select_removal_entries(entries, now, ttl, limit, recent_activity_grace, protected)
        .into_iter()
        .map(|entry| entry.path)
        .collect()
}

fn select_removal_entries(
    entries: &[StoredEntry],
    now: SystemTime,
    ttl: Duration,
    limit: usize,
    recent_activity_grace: Duration,
    protected: Option<&Path>,
) -> Vec<StoredEntry> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| right.path.cmp(&left.path))
    });
    let capacity = limit;
    let mut retained = 0usize;
    let mut removals = Vec::new();
    for entry in sorted {
        if protected.is_some_and(|path| entry.path == path) {
            continue;
        }
        let age = now.duration_since(entry.modified).ok();
        let recently_active = age.is_some_and(|age| age < recent_activity_grace);
        if recently_active {
            continue;
        }
        let stale = age.is_some_and(|age| age >= ttl);
        if stale || retained >= capacity {
            removals.push(entry);
        } else {
            retained += 1;
        }
    }
    removals
}

fn remove_unchanged_file(entry: &StoredEntry) -> Result<bool, StorageError> {
    let lock_path = entry.path.with_extension("jsonl.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| io_error("open cleanup lock", &lock_path, source))?;
    match lock_file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(source)) => {
            return Err(io_error("lock file for cleanup", &lock_path, source));
        }
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&entry.path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("open file for cleanup", &entry.path, source)),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(source)) => {
            return Err(io_error("lock file for cleanup", &entry.path, source));
        }
    }
    let metadata = file
        .metadata()
        .map_err(|source| io_error("read metadata for", &entry.path, source))?;
    let modified = metadata
        .modified()
        .map_err(|source| io_error("read modified time for", &entry.path, source))?;
    if modified != entry.modified {
        return Ok(false);
    }
    if remove_file(&entry.path)? {
        let _ = std::fs::remove_file(lock_path);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn remove_inactive_transcript(path: &Path) -> Result<bool, StorageError> {
    let lock_path = path.with_extension("jsonl.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| io_error("open transcript cleanup lock", &lock_path, source))?;
    match lock_file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(source)) => {
            return Err(io_error("lock transcript for cleanup", &lock_path, source));
        }
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error("open transcript for cleanup", path, source)),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(false),
        Err(std::fs::TryLockError::Error(source)) => {
            return Err(io_error("lock transcript for cleanup", path, source));
        }
    }
    if remove_file(path)? {
        let _ = std::fs::remove_file(lock_path);
        Ok(true)
    } else {
        Ok(false)
    }
}

fn remove_file(path: &Path) -> Result<bool, StorageError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("remove file", path, source)),
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("bingo-storage-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_at(path: &Path, modified: SystemTime) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "data").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(modified).unwrap();
    }

    fn test_policy(max_sessions: usize) -> RetentionPolicy {
        RetentionPolicy {
            ttl: Duration::from_secs(30 * 24 * 60 * 60),
            max_sessions,
            max_history_files: 2,
            recent_activity_grace: Duration::ZERO,
        }
    }

    #[test]
    fn windows_falls_back_to_userprofile_and_state_stays_under_it() {
        let profile = temp_home("windows-profile");
        let resolved =
            resolve_home_from(None, Some(profile.clone().into_os_string()), true).unwrap();

        assert_eq!(resolved, profile);
        for dir in [
            data_dir(&resolved),
            transcripts_dir(&resolved),
            history_dir(&resolved),
            shares_dir(&resolved),
            tasks_dir(&resolved),
        ] {
            assert!(
                dir.starts_with(&resolved),
                "{} escaped the home",
                dir.display()
            );
            assert!(dir.is_absolute(), "{} is relative", dir.display());
        }
        let _ = std::fs::remove_dir_all(profile);
    }

    #[test]
    fn home_wins_and_unix_does_not_treat_userprofile_as_home() {
        let home = temp_home("home-priority");
        let profile = temp_home("profile-secondary");
        assert_eq!(
            resolve_home_from(
                Some(home.clone().into_os_string()),
                Some(profile.clone().into_os_string()),
                true,
            )
            .unwrap(),
            home
        );
        assert!(matches!(
            resolve_home_from(None, Some(profile.clone().into_os_string()), false),
            Err(StorageError::HomeUnavailable)
        ));
        assert!(matches!(
            resolve_home_from(Some(OsString::new()), None, false),
            Err(StorageError::HomeUnavailable)
        ));
        let _ = std::fs::remove_dir_all(home);
        let _ = std::fs::remove_dir_all(profile);
    }

    #[test]
    fn summary_reports_the_policy_and_counts() {
        assert_eq!(
            CleanupReport::default().summary(),
            "no expired session data (TTL 30 days; latest 100 inactive sessions kept; 24-hour activity grace)"
        );
        assert_eq!(
            CleanupReport {
                transcripts: 2,
                shares: 1,
                history_files: 3,
            }
            .summary(),
            "cleaned 2 transcript(s), 1 share file(s), and 3 history file(s) (TTL 30 days; latest 100 inactive sessions kept; 24-hour activity grace)"
        );
    }

    #[test]
    fn default_policy_does_not_collect_recently_active_sessions_over_the_cap() {
        let home = temp_home("recent-activity");
        let now = SystemTime::now();
        for index in 0..=RETENTION_MAX_SESSIONS {
            write_at(
                &transcripts_dir(&home).join(format!("session-{index}.jsonl")),
                now - Duration::from_secs(index as u64),
            );
        }

        let report = cleanup_with_policy_at(&home, None, RetentionPolicy::default(), now).unwrap();

        assert_eq!(report.transcripts, 0);
        assert_eq!(collect_files(&transcripts_dir(&home)).unwrap().len(), 101);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn cleanup_applies_ttl_and_count_cap_while_protecting_current_session() {
        let home = temp_home("policy");
        let now = SystemTime::now();
        let current = transcripts_dir(&home).join("current.jsonl");
        let recent = transcripts_dir(&home).join("recent.jsonl");
        let overflow = transcripts_dir(&home).join("overflow.jsonl");
        let expired = transcripts_dir(&home).join("expired.jsonl");
        write_at(&current, now - Duration::from_secs(60 * 24 * 60 * 60));
        write_at(&recent, now - Duration::from_secs(60));
        write_at(&overflow, now - Duration::from_secs(120));
        write_at(&expired, now - Duration::from_secs(31 * 24 * 60 * 60));

        let report = cleanup_with_policy_at(&home, Some(&current), test_policy(1), now).unwrap();

        assert!(current.exists(), "the active session is never collected");
        assert!(recent.exists(), "the newest non-active session is retained");
        assert!(!overflow.exists(), "the count cap removes older overflow");
        assert!(!expired.exists(), "the TTL removes expired sessions");
        assert_eq!(report.transcripts, 2);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn cleanup_skips_a_transcript_locked_by_an_active_session() {
        let home = temp_home("active-lock");
        let now = SystemTime::now();
        let active = crate::transcript::create(&home, Path::new("project")).unwrap();
        active
            .append(&crate::api::types::Message::user_text("active"))
            .unwrap();
        let active_path = active.path().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&active_path)
            .unwrap();
        file.set_modified(now - Duration::from_secs(31 * 24 * 60 * 60))
            .unwrap();
        write_at(
            &shares_dir(&home).join(format!("{}.json", active.name())),
            now,
        );

        let report = cleanup_with_policy_at(&home, None, test_policy(0), now).unwrap();

        assert_eq!(report.transcripts, 0);
        assert_eq!(report.shares, 0);
        assert!(active_path.exists());
        assert!(
            shares_dir(&home)
                .join(format!("{}.json", active.name()))
                .exists()
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn cleanup_skips_a_transcript_when_only_its_data_file_is_locked() {
        let home = temp_home("active-data-lock");
        let now = SystemTime::now();
        let transcript = transcripts_dir(&home).join("renamed.jsonl");
        write_at(&transcript, now - Duration::from_secs(31 * 24 * 60 * 60));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&transcript)
            .unwrap();
        file.try_lock().unwrap();

        let report = cleanup_with_policy_at(&home, None, test_policy(0), now).unwrap();

        assert_eq!(report.transcripts, 0);
        assert!(transcript.exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn shares_follow_transcripts_and_history_are_bounded() {
        let home = temp_home("related");
        let now = SystemTime::now();
        let live = transcripts_dir(&home).join("live.jsonl");
        let expired = transcripts_dir(&home).join("expired.jsonl");
        write_at(&live, now);
        write_at(&expired, now - Duration::from_secs(31 * 24 * 60 * 60));
        write_at(&shares_dir(&home).join("live.json"), now);
        write_at(&shares_dir(&home).join("expired.json"), now);
        write_at(&shares_dir(&home).join("expired.json.bak"), now);
        write_at(&shares_dir(&home).join("orphan.json.bak"), now);
        for (name, age) in [("h1.jsonl", 1), ("h2.jsonl", 2), ("h3.jsonl", 3)] {
            write_at(
                &history_dir(&home).join(name),
                now - Duration::from_secs(age),
            );
        }

        let report = cleanup_with_policy_at(&home, Some(&live), test_policy(10), now).unwrap();

        assert!(shares_dir(&home).join("live.json").exists());
        assert!(!shares_dir(&home).join("expired.json").exists());
        assert!(!shares_dir(&home).join("expired.json.bak").exists());
        assert!(
            shares_dir(&home).join("orphan.json.bak").exists(),
            "unlinked share data is not deleted without lifecycle evidence"
        );
        assert_eq!(collect_files(&history_dir(&home)).unwrap().len(), 2);
        assert_eq!(report.transcripts, 1);
        assert_eq!(report.shares, 2);
        assert_eq!(report.history_files, 1);
        let _ = std::fs::remove_dir_all(home);
    }
}
