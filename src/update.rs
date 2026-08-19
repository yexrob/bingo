//! Version check + self-update (`bingo update`).
//!
//! Two consumers:
//! - **Startup check** (welcome-card data source): `spawn_background_check` runs at TUI session start,
//!   asynchronously fetches the latest GitHub release and writes a cache (24h TTL); network/parse failures
//!   are silent and never block startup; the welcome card reads [`latest_cached`].
//! - **`bingo update` command**: when run explicitly, network/verification failures are reported as-is (the user asked for it).
//!
//! Update flow: parse the latest tag → compare with the current version → download the platform asset (tar.gz / zip) +
//! `checksums.txt` for SHA-256 verification → extract the executable → atomic replace via tmp + rename
//! of [`std::env::current_exe`] (target path injectable for tests).

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;

/// The GitHub repo (yexrob/bingo) and the API / download base URL. The base is injectable:
/// tests point at a local mock server, production uses the defaults.
pub const REPO_OWNER: &str = "yexrob";
pub const REPO_NAME: &str = "bingo";
pub const RELEASE_API: &str = "https://api.github.com";
pub const DOWNLOAD_BASE: &str = "https://github.com";

/// Check-cache TTL: within 24h the GitHub API is not queried again (the startup path is silent; no traffic amplification).
pub const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Semver: a list of numeric parts (`v0.2.1` / `0.2.1` / `0.2.1-beta.1` all accepted,
/// each part takes its leading digits). Shorter versions are zero-padded in comparisons (`0.2.1 < 0.2.10`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    parts: Vec<u64>,
}

impl Version {
    /// Parse a version string like `v0.2.1`, `0.2.1`, or `0.2.1-beta.1`;
    /// empty / non-numeric parts return None. Pre-release/build suffixes (`-beta.1` / `+build`)
    /// are ignored — release tags are always stable, suffixes do not affect comparison.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let mut parts = Vec::new();
        for seg in s.split('.') {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            parts.push(digits.parse().ok()?);
            // This part carries a suffix (e.g. `1-beta`): ignore the rest of the part and later parts.
            if seg.chars().any(|c| !c.is_ascii_digit()) {
                break;
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(Version { parts })
    }

    /// The current binary's version (Cargo.toml `version`).
    #[allow(clippy::expect_used)]
    pub fn from_pkg() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("Cargo.toml version must be a semantic version")
    }

    /// The version's numeric parts (production only uses Display/comparison; tests assert on them).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn parts(&self) -> &[u64] {
        &self.parts
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            self.parts
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(".")
        )
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let n = self.parts.len().max(other.parts.len());
        for i in 0..n {
            let a = self.parts.get(i).copied().unwrap_or(0);
            let b = other.parts.get(i).copied().unwrap_or(0);
            match a.cmp(&b) {
                std::cmp::Ordering::Equal => continue,
                o => return o,
            }
        }
        std::cmp::Ordering::Equal
    }
}

/// Platform → prebuilt asset name mapping: `bingo-<triple>.tar.gz` (non-Windows) /
/// `bingo-<triple>.zip` (Windows). `os`/`arch` take the [`std::env::consts`] values
/// (macos / linux / windows; aarch64 / x86_64).
pub fn asset_name_for(os: &str, arch: &str) -> Option<String> {
    let triple = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    let ext = if os == "windows" { "zip" } else { "tar.gz" };
    Some(format!("bingo-{triple}.{ext}"))
}

/// The asset name for the current platform (None = no prebuilt asset, e.g. aarch64-linux outside macOS arm64).
pub fn current_asset_name() -> Option<String> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// The executable file name inside the archive (`.exe` on Windows).
pub fn binary_name(os: &str) -> &'static str {
    if os == "windows" {
        "bingo.exe"
    } else {
        "bingo"
    }
}

/// Resolve the expected hash (lowercase hex) for an asset from `checksums.txt` (`sha256sum` output:
/// `<hash>  <file>` or `<file>  <hash>`, file names may carry `*` / `./` prefixes).
pub fn checksum_for(checksums: &str, asset: &str) -> Option<String> {
    for line in checksums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (hash, file) = match cols.as_slice() {
            [h, f, ..] if is_sha256_hex(h) => (h, f),
            [f, h, ..] if is_sha256_hex(h) => (h, f),
            _ => continue,
        };
        let file = file.trim_start_matches('*').trim_start_matches("./");
        if file == asset {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// SHA-256 hex (lowercase).
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify the downloaded content against the matching asset entry in checksums.txt.
pub fn verify_checksum(data: &[u8], checksums: &str, asset: &str) -> Result<(), UpdateError> {
    let expected = checksum_for(checksums, asset).ok_or_else(|| {
        UpdateError::ChecksumsUnavailable(format!("{asset} not found in checksums.txt"))
    })?;
    let got = sha256_hex(data);
    if got == expected {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch { expected, got })
    }
}

/// Extract the executable bytes from the archive (tar.gz; Windows uses zip, see the cfg branches).
#[cfg(not(windows))]
fn extract_binary(archive: &[u8], bin_name: &str) -> Result<Vec<u8>, UpdateError> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let entries = tar
        .entries()
        .map_err(|e| UpdateError::ArchiveInvalid(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| UpdateError::ArchiveInvalid(e.to_string()))?;
        let path = entry
            .path()
            .map_err(|e| UpdateError::ArchiveInvalid(e.to_string()))?
            .into_owned();
        if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    Err(UpdateError::MissingBinary(bin_name.to_string()))
}

/// Extract the executable from the zip (`bingo.exe`; falls back to the first `.exe` entry).
#[cfg(windows)]
fn extract_binary(archive: &[u8], bin_name: &str) -> Result<Vec<u8>, UpdateError> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| UpdateError::ArchiveInvalid(e.to_string()))?;
    for i in 0..zip.len() {
        let mut f = zip
            .by_index(i)
            .map_err(|e| UpdateError::ArchiveInvalid(e.to_string()))?;
        if f.name().ends_with(bin_name) || f.name().ends_with(".exe") {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    Err(UpdateError::MissingBinary(bin_name.to_string()))
}

/// Atomically replace the executable: write a tmp file in the same dir (same filesystem) + rename.
/// Unix sets 0o755 explicitly (the in-tar mode may be lost to umask); Windows rename cannot
/// overwrite an existing target, so the old file is deleted first (best-effort, non-atomic — on Windows the user bears the window).
pub fn install_binary(bytes: &[u8], exe: &Path) -> Result<PathBuf, UpdateError> {
    let parent = exe
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let tmp = parent.join(format!(
        ".bingo-update-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(windows)]
    let _ = std::fs::remove_file(exe);
    std::fs::rename(&tmp, exe)?;
    Ok(exe.to_path_buf())
}

/// The latest GitHub release info.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: Version,
}

/// Fetch the latest GitHub release (`GET {api_base}/repos/{owner}/{repo}/releases/latest`),
/// parse `tag_name`. Called from the command path — network/parse failures are reported as-is.
pub async fn fetch_latest_release(
    client: &reqwest::Client,
    api_base: &str,
) -> Result<ReleaseInfo, UpdateError> {
    let url = format!("{api_base}/repos/{REPO_OWNER}/{REPO_NAME}/releases/latest");
    let resp = client
        .get(&url)
        .header(reqwest::header::USER_AGENT, "bingo-update")
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(UpdateError::Http {
            status: status.as_u16(),
        });
    }
    let json: serde_json::Value = resp.json().await?;
    let tag = json
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| UpdateError::BadResponse("release response is missing tag_name".into()))?
        .to_string();
    let version = Version::parse(&tag)
        .ok_or_else(|| UpdateError::BadResponse(format!("cannot parse tag {tag:?}")))?;
    Ok(ReleaseInfo { tag, version })
}

/// Fetch + write the cache; failures are silent (startup path: the check is an enhancement, not a contract).
/// `api_base` is injectable (mock server in tests; production passes [`RELEASE_API`]).
pub async fn fetch_and_cache(
    client: &reqwest::Client,
    home: &Path,
    api_base: &str,
) -> Option<ReleaseInfo> {
    match fetch_latest_release(client, api_base).await {
        Ok(info) => {
            write_cache(home, &info.tag);
            Some(info)
        }
        Err(_) => None,
    }
}

/// Warm the check at TUI session start: skip when the cache is fresh (within 24h), otherwise fetch in the background +
/// write the cache. Never blocks startup; not called on the headless (`--print`) path.
pub fn spawn_background_check(home: PathBuf) {
    if latest_cached(&home).is_some() {
        return;
    }
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let _ = fetch_and_cache(&client, &home, RELEASE_API).await;
    });
}

/// The check cache: `~/.local/share/bingo/update-check.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    /// Unix seconds (when the check ran).
    pub checked_at: u64,
    pub latest_tag: String,
}

pub fn cache_path(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("bingo")
        .join("update-check.json")
}

pub fn read_cache(home: &Path) -> Option<UpdateCache> {
    let raw = std::fs::read_to_string(cache_path(home)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn cache_fresh(c: &UpdateCache) -> bool {
    now_secs().saturating_sub(c.checked_at) < CACHE_TTL.as_secs()
}

/// The latest version in a fresh cache (welcome-card data source; missing/stale/corrupt cache → None).
pub fn latest_cached(home: &Path) -> Option<Version> {
    let c = read_cache(home)?;
    if !cache_fresh(&c) {
        return None;
    }
    // Only a strictly newer release is an update. Without this comparison the
    // welcome banner advertised whatever the cache held — including an OLDER
    // version right after upgrading (run_update writes the current version
    // into the cache on both "already latest" and "updated" outcomes).
    let latest = Version::parse(&c.latest_tag)?;
    if latest > Version::from_pkg() {
        Some(latest)
    } else {
        None
    }
}

/// Write the cache (tmp + rename; failures are silent — the startup path never errors on this).
pub fn write_cache(home: &Path, tag: &str) {
    let cache = UpdateCache {
        checked_at: now_secs(),
        latest_tag: tag.to_string(),
    };
    let path = cache_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&cache) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// The `bingo update` outcome.
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// Already up to date.
    Latest { current: Version },
    /// `--check` (dry-run): a newer version exists; nothing downloaded.
    Available { version: Version, tag: String },
    /// Installed the new version at the given path.
    Updated { version: Version, exe: PathBuf },
}

/// The core download + verify + extract + replace flow. `api_base`/`download_base`/`exe` are injectable
/// (mock server and temp target file in tests); production passes the defaults via [`run_update`].
pub async fn perform_update(
    client: &reqwest::Client,
    check_only: bool,
    api_base: &str,
    download_base: &str,
    exe: Option<&Path>,
) -> Result<UpdateOutcome, UpdateError> {
    let info = fetch_latest_release(client, api_base).await?;
    let current = Version::from_pkg();
    if info.version <= current {
        return Ok(UpdateOutcome::Latest { current });
    }
    if check_only {
        return Ok(UpdateOutcome::Available {
            version: info.version,
            tag: info.tag,
        });
    }
    let os = std::env::consts::OS;
    let asset = current_asset_name().ok_or_else(|| {
        UpdateError::UnsupportedPlatform(format!("{os}/{}", std::env::consts::ARCH))
    })?;
    let dl = |name: &str| {
        format!("{download_base}/{REPO_OWNER}/{REPO_NAME}/releases/latest/download/{name}")
    };
    let archive = download(client, &dl(&asset)).await?;
    let checksums = download(client, &dl("checksums.txt")).await?;
    let checksums = String::from_utf8(checksums)
        .map_err(|_| UpdateError::ChecksumsUnavailable("checksums.txt is not UTF-8 text".into()))?;
    verify_checksum(&archive, &checksums, &asset)?;
    let bin = extract_binary(&archive, binary_name(os))?;
    let exe = exe
        .map(Path::to_path_buf)
        .unwrap_or(std::env::current_exe()?);
    let installed = install_binary(&bin, &exe)?;
    Ok(UpdateOutcome::Updated {
        version: info.version,
        exe: installed,
    })
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, UpdateError> {
    let resp = client.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(UpdateError::Http {
            status: status.as_u16(),
        });
    }
    Ok(resp.bytes().await?.to_vec())
}

/// The `bingo update [--check]` command entry: also writes the cache on success (so the welcome card stops nagging).
pub async fn run_update(home: &Path, check_only: bool) -> Result<(), UpdateError> {
    let client = reqwest::Client::new();
    let outcome = perform_update(&client, check_only, RELEASE_API, DOWNLOAD_BASE, None).await?;
    match &outcome {
        UpdateOutcome::Latest { current } => {
            write_cache(home, &format!("v{current}"));
            println!("bingo is already the latest version v{current}");
        }
        UpdateOutcome::Available { version, tag } => {
            write_cache(home, tag);
            println!("a new version v{version} is available: run `bingo update` to install");
        }
        UpdateOutcome::Updated { version, exe } => {
            write_cache(home, &format!("v{version}"));
            println!("bingo updated to v{version}");
            println!(
                "installed at: {} (the new version takes effect on next launch)",
                exe.display()
            );
        }
    }
    Ok(())
}

/// Update errors. Stable error codes live in the [`ErrorCode`] impl (registered in `src/error.rs`).
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0} (check your connection and retry)")]
    Network(#[from] reqwest::Error),
    #[error("GitHub returned HTTP {status}")]
    Http { status: u16 },
    #[error("malformed release response: {0}")]
    BadResponse(String),
    #[error("cannot verify the update package: {0}")]
    ChecksumsUnavailable(String),
    #[error(
        "SHA-256 check failed (expected {expected}, got {got}): the download may have been tampered with; retry or install manually"
    )]
    ChecksumMismatch { expected: String, got: String },
    #[error("failed to unpack the update archive: {0}")]
    ArchiveInvalid(String),
    #[error("no executable {0} found in the update archive")]
    MissingBinary(String),
    #[error(
        "no prebuilt asset for the current platform {0} (bingo update does not support it yet; install manually)"
    )]
    UnsupportedPlatform(String),
    #[error(
        "update file operation failed: {0} (if it is a permission issue, use sudo bingo update or install manually)"
    )]
    Io(#[from] std::io::Error),
}

/// Pin the `Network → OFFLINE` mapping (`reqwest::Error` has no public constructor,
/// so the drift-guard test cannot enumerate that variant; same pattern as `api::client::transport_offline_code`).
#[doc(hidden)]
pub fn network_error_code() -> &'static str {
    "OFFLINE"
}

impl ErrorCode for UpdateError {
    /// Every variant maps explicitly to a stable code (new variants must add a mapping; the missing-`_` arm enforces it at compile time).
    fn error_code(&self) -> &'static str {
        match self {
            UpdateError::Network(_) => network_error_code(),
            UpdateError::Http { .. } => "OFFLINE",
            UpdateError::BadResponse(_) => "SERVER_ERROR",
            UpdateError::ChecksumsUnavailable(_) => "CHECKSUMS_UNAVAILABLE",
            UpdateError::ChecksumMismatch { .. } => "CHECKSUM_MISMATCH",
            UpdateError::ArchiveInvalid(_) | UpdateError::MissingBinary(_) => "ARCHIVE_INVALID",
            UpdateError::UnsupportedPlatform(_) => "UNSUPPORTED_PLATFORM",
            UpdateError::Io(_) => "STORAGE_ERROR",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bingo-update-test-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The banner only advertises strictly newer releases: a cache holding the
    /// current or an older version (the post-upgrade state) yields no banner.
    #[test]
    fn latest_cached_ignores_current_and_older_versions() {
        let home = tmp_home();
        let current = Version::from_pkg();

        write_cache(&home, &format!("v{current}"));
        assert_eq!(
            latest_cached(&home),
            None,
            "current version is not advertised"
        );

        write_cache(&home, "v0.0.1");
        assert_eq!(
            latest_cached(&home),
            None,
            "older versions are not advertised (post-upgrade cache state)"
        );

        let newer = format!("v{}.0.0", current.parts.first().copied().unwrap_or(0) + 1);
        write_cache(&home, &newer);
        assert_eq!(
            latest_cached(&home),
            Version::parse(&newer),
            "only a strictly newer version is advertised"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Build the tar.gz bytes containing the `bingo` executable.
    fn sample_tar_gz(bin_bytes: &[u8]) -> Vec<u8> {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(bin_bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "bingo", bin_bytes)
                .unwrap();
            builder.finish().unwrap();
        }
        enc.finish().unwrap()
    }

    /// Build an extractable release archive for the current platform (Windows → zip, others → tar.gz).
    fn sample_archive(bin_bytes: &[u8]) -> Vec<u8> {
        #[cfg(windows)]
        {
            use std::io::Write;
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            zip.start_file("bingo.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bin_bytes).unwrap();
            zip.finish().unwrap().into_inner()
        }
        #[cfg(not(windows))]
        {
            sample_tar_gz(bin_bytes)
        }
    }

    #[test]
    fn version_parse() {
        assert_eq!(Version::parse("0.2.1").unwrap().parts(), &[0, 2, 1]);
        assert_eq!(Version::parse("v0.2.1").unwrap().parts(), &[0, 2, 1]);
        assert_eq!(Version::parse("V1.2").unwrap().parts(), &[1, 2]);
        assert_eq!(Version::parse("0.2.1-beta.1").unwrap().parts(), &[0, 2, 1]);
        assert_eq!(Version::parse("1").unwrap().parts(), &[1]);
        assert!(Version::parse("").is_none());
        assert!(Version::parse("v").is_none());
        assert!(Version::parse("1.2.x").is_none());
        assert!(Version::parse(".1").is_none());
        assert!(Version::parse("1.").is_none());
    }

    #[test]
    fn version_cmp_pads_shorter() {
        assert!(Version::parse("0.2.1").unwrap() < Version::parse("0.2.2").unwrap());
        assert!(Version::parse("0.2.9").unwrap() < Version::parse("0.2.10").unwrap());
        assert!(Version::parse("0.2.1").unwrap() < Version::parse("0.3.0").unwrap());
        assert!(Version::parse("0.2.1").unwrap() < Version::parse("1.0.0").unwrap());
        // Shorter versions are zero-padded: 0.2.1 == 0.2.1.0 and 0.2 == 0.2.0 in comparisons (Eq compares field-by-field without padding, hence the cmp assertion)
        use std::cmp::Ordering;
        assert_eq!(
            Version::parse("0.2.1")
                .unwrap()
                .cmp(&Version::parse("0.2.1.0").unwrap()),
            Ordering::Equal
        );
        assert_eq!(
            Version::parse("0.2")
                .unwrap()
                .cmp(&Version::parse("0.2.0").unwrap()),
            Ordering::Equal
        );
        assert!(Version::parse("0.2").unwrap() > Version::parse("0.1.9").unwrap());
        // Suffixes are ignored: 0.2.1-beta.1 == 0.2.1 (pre-releases are not distinguished — release tags are always stable)
        assert_eq!(
            Version::parse("0.2.1-beta.1").unwrap(),
            Version::parse("0.2.1").unwrap()
        );
    }

    #[test]
    fn version_display() {
        assert_eq!(Version::parse("v0.2.1").unwrap().to_string(), "0.2.1");
    }

    #[test]
    fn asset_name_mapping() {
        assert_eq!(
            asset_name_for("macos", "aarch64").unwrap(),
            "bingo-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name_for("macos", "x86_64").unwrap(),
            "bingo-x86_64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name_for("linux", "x86_64").unwrap(),
            "bingo-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            asset_name_for("windows", "x86_64").unwrap(),
            "bingo-x86_64-pc-windows-msvc.zip"
        );
        assert!(asset_name_for("linux", "aarch64").is_none());
        assert!(asset_name_for("freebsd", "x86_64").is_none());
        assert!(asset_name_for("windows", "arm64").is_none());
        // The current platform must map to one of the prebuilt assets (bingo's release matrix).
        assert!(
            current_asset_name().is_some(),
            "the current platform must be in the release matrix"
        );
        assert_eq!(binary_name("windows"), "bingo.exe");
        assert_eq!(binary_name("macos"), "bingo");
    }

    #[test]
    fn checksum_parse_both_column_orders() {
        let hash = "a".repeat(64);
        // sha256sum canonical format: hash + two spaces + file name
        let checksums = format!("{hash}  bingo-aarch64-apple-darwin.tar.gz\n");
        assert_eq!(
            checksum_for(&checksums, "bingo-aarch64-apple-darwin.tar.gz"),
            Some(hash.clone())
        );
        // File name first (some release-tool formats)
        let reverse = format!("bingo-aarch64-apple-darwin.tar.gz  {hash}\n");
        assert_eq!(
            checksum_for(&reverse, "bingo-aarch64-apple-darwin.tar.gz"),
            Some(hash)
        );
    }

    #[test]
    fn checksum_parse_prefixes_and_case() {
        let h = "abcdef0123456789".repeat(4); // 64 chars
        // `*` binary marker + `./` prefix + mixed case
        let checksums = format!("{h}  *./bingo-x86_64-apple-darwin.tar.gz\n");
        assert_eq!(
            checksum_for(&checksums, "bingo-x86_64-apple-darwin.tar.gz"),
            Some(h.to_ascii_lowercase())
        );
        let upper = h.to_ascii_uppercase();
        let checksums2 = format!("bingo-x86_64-apple-darwin.tar.gz  {upper}\n");
        assert_eq!(
            checksum_for(&checksums2, "bingo-x86_64-apple-darwin.tar.gz"),
            Some(h.to_ascii_lowercase())
        );
        // Comments / blank lines are skipped
        let with_noise = format!("# generated by release tool\n\n{h}  bingo.tar.gz\n");
        assert_eq!(checksum_for(&with_noise, "bingo.tar.gz"), Some(h));
    }

    #[test]
    fn checksum_verify_ok_and_mismatch() {
        let data = b"payload-bytes";
        let asset = "bingo-x86_64-unknown-linux-gnu.tar.gz";
        let good = sha256_hex(data);
        let checksums = format!("{good}  {asset}\n");
        assert!(verify_checksum(data, &checksums, asset).is_ok());

        // Hash mismatch → ChecksumMismatch
        let bad = format!("{}  {asset}\n", "f".repeat(64));
        assert!(matches!(
            verify_checksum(data, &bad, asset),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
        // Missing entry → ChecksumsUnavailable
        assert!(matches!(
            verify_checksum(data, &format!("{good}  other.bin\n"), asset),
            Err(UpdateError::ChecksumsUnavailable(_))
        ));
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn extract_binary_from_archive() {
        let bin = b"#!/bin/sh\necho bingo\n";
        let archive = sample_archive(bin);
        assert_eq!(extract_binary(&archive, "bingo").unwrap(), bin);
        // Missing entry → MissingBinary (the Windows .exe fallback match would hit bingo.exe,
        // so verify with an archive that has no .exe entry)
        #[cfg(windows)]
        let no_bin = {
            use std::io::Write;
            let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            z.start_file("readme.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"hi").unwrap();
            z.finish().unwrap().into_inner()
        };
        #[cfg(not(windows))]
        let no_bin = archive.clone();
        assert!(matches!(
            extract_binary(&no_bin, "nope"),
            Err(UpdateError::MissingBinary(_))
        ));
        // Corrupt → ArchiveInvalid (Windows zip tolerates arbitrary short bytes, so build a truncated zip header)
        #[cfg(windows)]
        let corrupt: &[u8] = &[0x50, 0x4B, 0x03, 0x04, 0x00, 0x00]; // zip local header + truncation
        #[cfg(not(windows))]
        let corrupt: &[u8] = b"not a gzip";
        assert!(matches!(
            extract_binary(corrupt, "bingo"),
            Err(UpdateError::ArchiveInvalid(_))
        ));
    }

    #[test]
    fn install_binary_replaces_atomically() {
        let home = tmp_home();
        let exe = home.join("bin").join("bingo");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"old").unwrap();
        install_binary(b"new-bytes", &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new-bytes");
        // No tmp-file leftovers
        let leftovers: Vec<_> = std::fs::read_dir(home.join("bin"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".bingo-update-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp files should have been renamed away: {leftovers:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "the executable bit should be kept");
        }
    }

    #[test]
    fn cache_roundtrip_and_ttl() {
        let home = tmp_home();
        assert!(latest_cached(&home).is_none(), "no cache → None");
        write_cache(&home, "v0.9.9");
        assert_eq!(latest_cached(&home).unwrap().parts(), &[0, 9, 9]);
        assert_eq!(read_cache(&home).unwrap().checked_at, now_secs());

        // Stale cache (written before the TTL) → None
        let c = UpdateCache {
            checked_at: now_secs() - CACHE_TTL.as_secs() - 1,
            latest_tag: "v0.9.9".into(),
        };
        std::fs::write(cache_path(&home), serde_json::to_string(&c).unwrap()).unwrap();
        assert!(
            latest_cached(&home).is_none(),
            "a stale cache should count as no check result"
        );
        // Corrupt cache → None (no panic)
        std::fs::write(cache_path(&home), "not json").unwrap();
        assert!(latest_cached(&home).is_none());
    }

    /// Minimal HTTP mock: returns a preset response per full path.
    async fn serve(routes: Vec<(&'static str, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let routes = std::sync::Arc::new(routes);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.split_whitespace().nth(1).unwrap_or("/");
                    let (status, body) = match routes.iter().find(|(p, _)| p == &path) {
                        Some((_, body)) => ("200 OK", body.clone()),
                        None => ("404 Not Found", b"not found".to_vec()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Build the full mock routes (release JSON + asset + checksums.txt).
    /// `tag` is the release tag; returns (routes, archive).
    fn mock_routes(tag: &str, archive: Vec<u8>) -> (Vec<(&'static str, Vec<u8>)>, Vec<u8>) {
        let release = format!(r#"{{"tag_name":"{tag}","name":"bingo {tag}","assets":[]}}"#,);
        let asset = current_asset_name().expect("the current platform is in the release matrix");
        let checksum = sha256_hex(&archive);
        let checksums = format!("{checksum}  {asset}\n");
        (
            vec![
                ("/repos/yexrob/bingo/releases/latest", release.into_bytes()),
                (
                    Box::leak(
                        format!("/yexrob/bingo/releases/latest/download/{asset}").into_boxed_str(),
                    ),
                    archive.clone(),
                ),
                (
                    "/yexrob/bingo/releases/latest/download/checksums.txt",
                    checksums.into_bytes(),
                ),
            ],
            archive,
        )
    }

    #[tokio::test]
    async fn perform_update_end_to_end() {
        let bin = b"BINGO-BIN-2026";
        let (routes, _) = mock_routes("v99.0.0", sample_archive(bin));
        let base = serve(routes).await;

        let home = tmp_home();
        let exe = home.join("bingo");
        std::fs::write(&exe, b"old-binary").unwrap();

        let client = reqwest::Client::new();
        let outcome = perform_update(&client, false, &base, &base, Some(&exe))
            .await
            .expect("the mock server should run the whole flow successfully");
        match outcome {
            UpdateOutcome::Updated { version, exe } => {
                assert_eq!(version.parts(), &[99, 0, 0]);
                assert_eq!(std::fs::read(&exe).unwrap(), bin);
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_check_only_skips_download() {
        let (routes, _) = mock_routes("v99.0.0", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let client = reqwest::Client::new();
        let outcome = perform_update(&client, true, &base, &base, None)
            .await
            .expect("--check only parses the release; must not fail");
        match outcome {
            UpdateOutcome::Available { version, .. } => {
                assert_eq!(version.parts(), &[99, 0, 0]);
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_latest_when_version_not_newer() {
        let (routes, _) = mock_routes("v0.0.1", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let client = reqwest::Client::new();
        let outcome = perform_update(&client, false, &base, &base, None)
            .await
            .expect("an older tag → Latest must not fail");
        match outcome {
            UpdateOutcome::Latest { current } => {
                assert_eq!(current, Version::from_pkg());
            }
            other => panic!("expected Latest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_checksum_mismatch_aborts() {
        let archive = sample_tar_gz(b"x");
        let release = r#"{"tag_name":"v99.0.0","name":"bingo v99.0.0","assets":[]}"#.to_string();
        let asset = current_asset_name().unwrap();
        // checksums.txt carries a wrong hash → verification fails, the executable must not be replaced
        let bad_checksum = "f".repeat(64);
        let checksums = format!("{bad_checksum}  {asset}\n");
        let base = serve(vec![
            ("/repos/yexrob/bingo/releases/latest", release.into_bytes()),
            (
                Box::leak(
                    format!("/yexrob/bingo/releases/latest/download/{asset}").into_boxed_str(),
                ),
                archive,
            ),
            (
                "/yexrob/bingo/releases/latest/download/checksums.txt",
                checksums.into_bytes(),
            ),
        ])
        .await;
        let home = tmp_home();
        let exe = home.join("bingo");
        std::fs::write(&exe, b"keep-me").unwrap();
        let client = reqwest::Client::new();
        let err = perform_update(&client, false, &base, &base, Some(&exe))
            .await
            .expect_err("verification failure must error");
        assert!(matches!(err, UpdateError::ChecksumMismatch { .. }));
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"keep-me",
            "a failure must not touch the existing binary"
        );
        assert_eq!(err.error_code(), "CHECKSUM_MISMATCH");
    }

    #[tokio::test]
    async fn perform_update_http_error_propagates() {
        // Empty routes: every path returns 404 → the release fetch reports Http{404}
        let base = serve(vec![]).await;
        let client = reqwest::Client::new();
        let err = perform_update(&client, false, &base, &base, None)
            .await
            .expect_err("a 404 should report an Http error");
        assert!(matches!(err, UpdateError::Http { status: 404 }));
        assert_eq!(err.error_code(), "OFFLINE");
    }

    #[tokio::test]
    async fn fetch_and_cache_writes_cache_and_silent_failure() {
        // Success path: the cache is written (api_base points at the mock, no real GitHub calls)
        let (routes, _) = mock_routes("v3.1.4", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let home = tmp_home();
        let client = reqwest::Client::new();
        let got = fetch_and_cache(&client, &home, &base).await;
        assert!(got.is_some());
        assert_eq!(latest_cached(&home).unwrap().parts(), &[3, 1, 4]);

        // Failure path: 404 → silent None (no cache write, no panic)
        let dead = serve(vec![]).await;
        let home2 = tmp_home();
        assert!(fetch_and_cache(&client, &home2, &dead).await.is_none());
        assert!(latest_cached(&home2).is_none());
    }
}
