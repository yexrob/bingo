//! A picture fetched from the web, kept on this machine.
//!
//! A URL an answer named is read again every time a session shows it and again
//! every time one is resumed: the same bytes over the same wire, for a picture
//! that has not changed (user-reported, 2026-09-04). So a fetched picture is
//! written under the data directory and read back out of it while it is young
//! enough.
//!
//! **When it was fetched is the file's own modification time**, and nothing
//! beside it: a sidecar saying the same thing is a second fact to keep in step.
//! The name is a hash of the address, so one address is one entry and two are
//! two, whatever their length or their punctuation.
//!
//! Two bingo processes share the directory, so an entry is written under a name
//! of that write's own and renamed into place — a reader can never see half a
//! picture. And a cache that cannot be written is a cache that is not used: the
//! picture still shows.
//!
//! Nothing sweeps the directory: a stale entry is removed as it is passed over,
//! which is the only moment anything here knows an address was asked for.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The directory this machine keeps pictures in, under the data directory. The
/// cache is below it; the file a surface writes for a viewer is beside it
/// (M56).
pub const DIR: &str = "pictures";

/// The cache's own directory under [`DIR`].
const CACHE: &str = "cache";

/// How long a fetched picture is kept unless a person says otherwise: two
/// weeks. Long enough that a conversation somebody comes back to draws without
/// a fetch, short enough that a picture which changed behind its address is not
/// shown for a month.
pub const DAYS: u64 = 14;

const SECONDS_A_DAY: u64 = 24 * 60 * 60;

/// Where fetched pictures are kept, and for how long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cache {
    dir: PathBuf,
    ttl: Duration,
}

impl Cache {
    /// The cache under a data directory, keeping a picture `days` days.
    ///
    /// `None` for `0`: a cache of no days is no cache, and then nothing is
    /// written, nothing is read, and the directory is never made.
    pub fn under(data_dir: &Path, days: u64) -> Option<Self> {
        (days > 0).then(|| Self {
            dir: data_dir.join(DIR).join(CACHE),
            ttl: Duration::from_secs(days.saturating_mul(SECONDS_A_DAY)),
        })
    }

    /// Where `url`'s entry is, whether it is there or not.
    pub fn path(&self, url: &str) -> PathBuf {
        self.dir.join(named(url))
    }

    /// The bytes kept for `url`, where an entry is there and young enough.
    ///
    /// A stale one is removed as it is passed over. An entry that will not read
    /// — a directory in its place, a permission gone — is a miss, never an
    /// error: what the caller wants is the picture, and the wire still has it.
    pub fn hit(&self, url: &str) -> Option<Vec<u8>> {
        let path = self.path(url);
        let mtime = std::fs::metadata(&path).and_then(|at| at.modified()).ok()?;
        if !fresh(mtime, SystemTime::now(), self.ttl) {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        std::fs::read(&path).ok()
    }

    /// Keep these bytes for `url`. Nothing is reported: the picture is in the
    /// caller's hand already, so a directory that will not take a copy of it is
    /// not something a person needs to hear about.
    pub fn keep(&self, url: &str, bytes: &[u8]) {
        let _ = self.written(url, bytes);
    }

    /// Through a temporary name and a rename, which is what makes two writers
    /// safe: the entry appears whole or not at all.
    fn written(&self, url: &str, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let temporary = self.dir.join(temporary(url));
        std::fs::write(&temporary, bytes)?;
        std::fs::rename(&temporary, self.path(url)).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary);
        })
    }
}

/// Whether an entry written at `mtime` may still be read at `now`.
///
/// An entry from the future is fresh: a clock that went backwards, or a file
/// system whose stamps are coarser than this machine's clock, is not a reason
/// to fetch the same picture again.
pub fn fresh(mtime: SystemTime, now: SystemTime, ttl: Duration) -> bool {
    now.duration_since(mtime)
        .map(|age| age < ttl)
        .unwrap_or(true)
}

/// The name of `url`'s entry: a hash of the address in hex, so the name is
/// short, is a name on every file system, and is the same on every run.
///
/// FNV-1a over 128 bits, spelled here because no digest of that width is in
/// this workspace's dependency tree and one crate over the budget is one too
/// many (`scripts/budget.toml`). It has one job — telling two addresses apart —
/// and 128 bits is far more than a cache of a few hundred entries can collide
/// in. It is not a signature and nothing here treats it as one.
fn named(url: &str) -> String {
    format!("{:032x}", hashed(url))
}

/// The name one write uses before its rename: the entry's own name, this
/// process, and a number no other write in it repeats — two processes, or two
/// tests, must not rename each other's half-written file into place.
fn temporary(url: &str) -> String {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    format!("{}.{}.{n}.tmp", named(url), std::process::id())
}

const FNV_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

fn hashed(text: &str) -> u128 {
    text.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(dir: &Path) -> Cache {
        Cache::under(dir, DAYS).expect("a fortnight is a cache")
    }

    /// The one fact about time, with the clock handed in.
    #[test]
    fn an_entry_is_fresh_until_its_time_is_up() {
        let ttl = Duration::from_secs(60);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(fresh(now, now, ttl), "written this instant");
        assert!(fresh(now - Duration::from_secs(59), now, ttl));
        assert!(!fresh(now - Duration::from_secs(60), now, ttl), "its time");
        assert!(!fresh(now - Duration::from_secs(600), now, ttl));
        assert!(
            fresh(now + Duration::from_secs(600), now, ttl),
            "a stamp from the future is a clock's business, not a reason to fetch"
        );
    }

    /// One address is one name, two addresses are two, and the name is a name
    /// on any file system however the address is punctuated.
    #[test]
    fn an_address_names_one_entry_and_no_other_address_names_it() {
        let dir = std::path::Path::new("/data");
        let cache = cache(dir);
        let one = cache.path("https://example.com/a/very/long/shot.png?x=1&y=2#z");
        assert_eq!(
            one,
            cache.path("https://example.com/a/very/long/shot.png?x=1&y=2#z")
        );
        assert_ne!(
            one,
            cache.path("https://example.com/a/very/long/shot.png?x=1&y=3#z")
        );
        assert_ne!(one, cache.path("https://example.com/b"));
        let name = one
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("a name");
        assert_eq!(name.len(), 32, "{name}");
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        assert_eq!(one.parent(), Some(dir.join(DIR).join(CACHE).as_path()));
    }

    #[test]
    fn a_cache_of_no_days_is_no_cache_at_all() {
        assert!(Cache::under(std::path::Path::new("/data"), 0).is_none());
        assert!(Cache::under(std::path::Path::new("/data"), 1).is_some());
    }

    /// A great many days is a long life, not an overflow.
    #[test]
    fn a_life_no_clock_could_measure_is_kept_rather_than_wrapped() {
        let cache = Cache::under(std::path::Path::new("/data"), u64::MAX).expect("a cache");
        assert!(fresh(
            SystemTime::UNIX_EPOCH,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1 << 40),
            cache.ttl
        ));
    }

    #[test]
    fn what_is_kept_is_read_back_and_nothing_is_left_beside_it() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = cache(dir.path());
        assert!(cache.hit("https://x/y.png").is_none(), "nothing yet");
        cache.keep("https://x/y.png", b"a picture");
        assert_eq!(
            cache.hit("https://x/y.png").as_deref(),
            Some(&b"a picture"[..])
        );
        let kept: Vec<_> = std::fs::read_dir(dir.path().join(DIR).join(CACHE))
            .expect("the directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(kept.len(), 1, "the entry and nothing beside it: {kept:?}");
        assert!(!kept[0].ends_with(".tmp"), "{kept:?}");
    }

    /// The sweep: an entry past its time is removed as it is passed over, so a
    /// directory nothing walks does not keep what nobody wants.
    #[test]
    fn an_entry_past_its_time_is_a_miss_and_is_gone() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = Cache::under(dir.path(), 1).expect("a day is a cache");
        cache.keep("https://x/y.png", b"a picture");
        aged(
            &cache.path("https://x/y.png"),
            Duration::from_secs(2 * SECONDS_A_DAY),
        );
        assert!(cache.hit("https://x/y.png").is_none(), "past its day");
        assert!(!cache.path("https://x/y.png").exists(), "and swept");
    }

    /// A cache under a path that cannot be made is a cache that is not used.
    #[test]
    fn a_directory_that_will_not_be_made_is_no_ones_error() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, b"in the way").expect("a file");
        let cache = cache(&file);
        cache.keep("https://x/y.png", b"a picture");
        assert!(cache.hit("https://x/y.png").is_none());
    }

    /// Written again is written over, and read back as the newer bytes.
    #[test]
    fn keeping_an_address_again_replaces_what_was_there() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = cache(dir.path());
        cache.keep("https://x/y.png", b"first");
        cache.keep("https://x/y.png", b"second");
        assert_eq!(
            cache.hit("https://x/y.png").as_deref(),
            Some(&b"second"[..])
        );
    }

    /// Set a file's modification time back by `age`, which is how a test makes
    /// an entry old without a sleep and without pinning a clock.
    fn aged(path: &Path, age: Duration) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("the entry");
        let when = SystemTime::now() - age;
        file.set_modified(when).expect("a stamp this machine takes");
    }
}
