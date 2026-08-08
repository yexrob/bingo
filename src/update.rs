//! 版本检测 + 自更新（`bingo update`）。
//!
//! 两个消费方：
//! - **启动检测**（欢迎卡片数据源）：`spawn_background_check` 在 TUI 会话启动时
//!   异步拉取 GitHub Releases latest 并写入缓存（24h TTL），网络/解析失败静默，
//!   不阻塞启动；欢迎卡片渲染读取 [`latest_cached`]。
//! - **`bingo update` 命令**：显式执行时网络/校验失败如实报错（用户主动触发）。
//!
//! 更新流程：解析最新 tag → 对比当前版本 → 下载平台资产（tar.gz / zip）+
//! `checksums.txt` 校验 SHA-256 → 解压出可执行文件 → tmp + rename 原子替换
//! [`std::env::current_exe`]（可注入目标路径，测试用）。

use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;

/// GitHub 仓库（yexrob/bingo）与 API / 下载基址。基址可注入：
/// 测试指向本地 mock 服务器，生产用默认值。
pub const REPO_OWNER: &str = "yexrob";
pub const REPO_NAME: &str = "bingo";
pub const RELEASE_API: &str = "https://api.github.com";
pub const DOWNLOAD_BASE: &str = "https://github.com";

/// 检测缓存 TTL：24h 内不重复请求 GitHub API（启动路径静默，无流量放大）。
pub const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// 语义版本：数字段列表（`v0.2.1` / `0.2.1` / `0.2.1-beta.1` 均接受，
/// 每段取前导数字）。比较时短者补 0（`0.2.1 < 0.2.10`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    parts: Vec<u64>,
}

impl Version {
    /// 解析形如 `v0.2.1`、`0.2.1`、`0.2.1-beta.1` 的版本串；
    /// 空段 / 非数字段返回 None。预发布/构建后缀（`-beta.1` / `+build`）
    /// 忽略——发布 tag 均为正式版，后缀不影响比较。
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches(['v', 'V']);
        let mut parts = Vec::new();
        for seg in s.split('.') {
            let digits: String = seg.chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            parts.push(digits.parse().ok()?);
            // 该段带后缀（如 `1-beta`）：忽略段内剩余与后续段。
            if seg.chars().any(|c| !c.is_ascii_digit()) {
                break;
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(Version { parts })
    }

    /// 当前二进制版本（Cargo.toml `version`）。
    pub fn from_pkg() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml version 必须是语义版本")
    }

    /// 版本数字段（生产只用 Display/比较；测试断言用）。
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

/// 平台 → 预编译资产名映射：`bingo-<triple>.tar.gz`（非 Windows）/
/// `bingo-<triple>.zip`（Windows）。`os`/`arch` 取 [`std::env::consts`] 的取值
/// （macos / linux / windows；aarch64 / x86_64）。
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

/// 当前平台的资产名（None = 无预编译资产，如 macOS arm64 之外的 aarch64-linux）。
pub fn current_asset_name() -> Option<String> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// 压缩包内可执行文件名（Windows 带 `.exe`）。
pub fn binary_name(os: &str) -> &'static str {
    if os == "windows" {
        "bingo.exe"
    } else {
        "bingo"
    }
}

/// 从 `checksums.txt`（`sha256sum` 输出：`<hash>  <file>` 或 `<file>  <hash>`，
/// 文件名可带 `*` / `./` 前缀）解析指定资产的期望哈希（小写 hex）。
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

/// SHA-256 hex（小写）。
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 校验下载内容与 checksums.txt 中对应资产条目一致。
pub fn verify_checksum(data: &[u8], checksums: &str, asset: &str) -> Result<(), UpdateError> {
    let expected = checksum_for(checksums, asset).ok_or_else(|| {
        UpdateError::ChecksumsUnavailable(format!("checksums.txt 中未找到 {asset}"))
    })?;
    let got = sha256_hex(data);
    if got == expected {
        Ok(())
    } else {
        Err(UpdateError::ChecksumMismatch { expected, got })
    }
}

/// 从压缩包解出可执行文件字节（tar.gz；Windows 为 zip，见 cfg 分支）。
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

/// 从 zip 解出可执行文件（`bingo.exe`；回退到第一个 `.exe` 条目）。
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

/// 原子替换可执行文件：写同目录 tmp（同文件系统）+ rename。
/// Unix 显式置 0o755（tar 内 mode 可能因 umask 丢失）；Windows rename 不能
/// 覆盖已存在目标，先删旧（尽力而为，非原子——Windows 上由用户承担窗口期）。
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

/// GitHub 最新 release 信息。
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: Version,
}

/// 拉取 GitHub Releases latest（`GET {api_base}/repos/{owner}/{repo}/releases/latest`），
/// 解析 `tag_name`。命令路径调用——网络/解析失败如实报错。
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
        .ok_or_else(|| UpdateError::BadResponse("release 响应缺少 tag_name".into()))?
        .to_string();
    let version = Version::parse(&tag)
        .ok_or_else(|| UpdateError::BadResponse(format!("无法解析 tag {tag:?}")))?;
    Ok(ReleaseInfo { tag, version })
}

/// 拉取 + 写缓存；失败静默（启动路径：检测是增强不是契约）。
/// `api_base` 可注入（测试用 mock 服务器；生产传 [`RELEASE_API`]）。
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

/// TUI 会话启动时预热检测：缓存新鲜（24h 内）直接跳过，否则后台拉取 +
/// 写缓存。不阻塞启动；headless（`--print`）路径不调用。
pub fn spawn_background_check(home: PathBuf) {
    if latest_cached(&home).is_some() {
        return;
    }
    tokio::spawn(async move {
        let client = reqwest::Client::new();
        let _ = fetch_and_cache(&client, &home, RELEASE_API).await;
    });
}

/// 检测缓存：`~/.local/share/bingo/update-check.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCache {
    /// Unix 秒（检查时间）。
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

/// 新鲜缓存中的最新版本（欢迎卡片数据源；缓存缺失/过期/损坏 → None）。
pub fn latest_cached(home: &Path) -> Option<Version> {
    let c = read_cache(home)?;
    if !cache_fresh(&c) {
        return None;
    }
    Version::parse(&c.latest_tag)
}

/// 写缓存（tmp + rename；失败静默——启动路径不因此报错）。
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

/// `bingo update` 执行结果。
#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// 当前已是最新。
    Latest { current: Version },
    /// `--check`（dry-run）：发现新版本，未下载。
    Available { version: Version, tag: String },
    /// 已安装新版本到指定路径。
    Updated { version: Version, exe: PathBuf },
}

/// 下载 + 校验 + 解压 + 替换核心流程。`api_base`/`download_base`/`exe` 可注入
/// （测试用 mock 服务器与临时目标文件）；生产由 [`run_update`] 传默认值。
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
        .map_err(|_| UpdateError::ChecksumsUnavailable("checksums.txt 非 UTF-8 文本".into()))?;
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

/// `bingo update [--check]` 命令入口：成功时顺带写缓存（欢迎卡片不再重复提示）。
pub async fn run_update(home: &Path, check_only: bool) -> Result<(), UpdateError> {
    let client = reqwest::Client::new();
    let outcome = perform_update(&client, check_only, RELEASE_API, DOWNLOAD_BASE, None).await?;
    match &outcome {
        UpdateOutcome::Latest { current } => {
            write_cache(home, &format!("v{current}"));
            println!("bingo 已是最新版本 v{current}");
        }
        UpdateOutcome::Available { version, tag } => {
            write_cache(home, tag);
            println!("发现新版本 v{version}：运行 `bingo update` 安装");
        }
        UpdateOutcome::Updated { version, exe } => {
            write_cache(home, &format!("v{version}"));
            println!("bingo 已更新到 v{version}");
            println!("安装位置：{}（新版本在下次启动时生效）", exe.display());
        }
    }
    Ok(())
}

/// 更新错误。稳定错误码见 [`ErrorCode`] impl（`src/error.rs` 登记）。
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("网络错误：{0}（请检查网络连接后重试）")]
    Network(#[from] reqwest::Error),
    #[error("GitHub 返回 HTTP {status}")]
    Http { status: u16 },
    #[error("release 响应异常：{0}")]
    BadResponse(String),
    #[error("无法校验更新包：{0}")]
    ChecksumsUnavailable(String),
    #[error("SHA-256 校验失败（期望 {expected}，实际 {got}）：下载可能被篡改，请重试或手动安装")]
    ChecksumMismatch { expected: String, got: String },
    #[error("更新包解压失败：{0}")]
    ArchiveInvalid(String),
    #[error("更新包中未找到可执行文件 {0}")]
    MissingBinary(String),
    #[error("当前平台 {0} 没有预编译资产（bingo update 暂不支持，请手动安装）")]
    UnsupportedPlatform(String),
    #[error("更新文件操作失败：{0}（若为权限问题，请用 sudo bingo update 或手动安装）")]
    Io(#[from] std::io::Error),
}

/// 锁定 `Network → OFFLINE` 映射（`reqwest::Error` 无公开构造器，
/// drift-guard 测试无法枚举该 variant；与 `api::client::transport_offline_code` 同模式）。
#[doc(hidden)]
pub fn network_error_code() -> &'static str {
    "OFFLINE"
}

impl ErrorCode for UpdateError {
    /// 全 variant 显式映射稳定码（新增 variant 必须补 mapping，无 `_` arm 编译期保证）。
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

    /// 构造含 `bingo` 可执行文件的 tar.gz 字节。
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

    /// 按当前平台构造可解压的发布归档（Windows → zip，其他 → tar.gz）。
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
        // 短者补 0：比较上 0.2.1 == 0.2.1.0、0.2 == 0.2.0（Eq 逐字段不补零，故用 cmp 断言）
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
        // 后缀被忽略：0.2.1-beta.1 == 0.2.1（预发布不区分——发布 tag 均为正式版）
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
        // 当前平台必须映射到预编译资产之一（bingo 的发布矩阵）。
        assert!(current_asset_name().is_some(), "当前平台应在发布矩阵内");
        assert_eq!(binary_name("windows"), "bingo.exe");
        assert_eq!(binary_name("macos"), "bingo");
    }

    #[test]
    fn checksum_parse_both_column_orders() {
        let hash = "a".repeat(64);
        // sha256sum 常规格式：hash + 双空格 + 文件名
        let checksums = format!("{hash}  bingo-aarch64-apple-darwin.tar.gz\n");
        assert_eq!(
            checksum_for(&checksums, "bingo-aarch64-apple-darwin.tar.gz"),
            Some(hash.clone())
        );
        // 文件名在前（部分发布工具格式）
        let reverse = format!("bingo-aarch64-apple-darwin.tar.gz  {hash}\n");
        assert_eq!(
            checksum_for(&reverse, "bingo-aarch64-apple-darwin.tar.gz"),
            Some(hash)
        );
    }

    #[test]
    fn checksum_parse_prefixes_and_case() {
        let h = "abcdef0123456789".repeat(4); // 64 chars
        // `*` binary 标记 + `./` 前缀 + 大小写混合
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
        // 注释 / 空行跳过
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

        // 哈希不符 → ChecksumMismatch
        let bad = format!("{}  {asset}\n", "f".repeat(64));
        assert!(matches!(
            verify_checksum(data, &bad, asset),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
        // 条目缺失 → ChecksumsUnavailable
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
        // 缺条目 → MissingBinary（Windows 的 .exe 兜底匹配会命中 bingo.exe，
        // 需用不含 .exe 条目的归档验证）
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
        // 损坏 → ArchiveInvalid（Windows zip 对任意短字节容错，需构造截断 zip 头）
        #[cfg(windows)]
        let corrupt: &[u8] = &[0x50, 0x4B, 0x03, 0x04, 0x00, 0x00]; // zip 局部头+截断
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
        // 无残留 tmp 文件
        let leftovers: Vec<_> = std::fs::read_dir(home.join("bin"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".bingo-update-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "tmp 文件应被 rename 掉: {leftovers:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&exe).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755, "可执行位应保留");
        }
    }

    #[test]
    fn cache_roundtrip_and_ttl() {
        let home = tmp_home();
        assert!(latest_cached(&home).is_none(), "无缓存 → None");
        write_cache(&home, "v0.9.9");
        assert_eq!(latest_cached(&home).unwrap().parts(), &[0, 9, 9]);
        assert_eq!(read_cache(&home).unwrap().checked_at, now_secs());

        // 过期缓存（TTL 之前写入）→ None
        let c = UpdateCache {
            checked_at: now_secs() - CACHE_TTL.as_secs() - 1,
            latest_tag: "v0.9.9".into(),
        };
        std::fs::write(cache_path(&home), serde_json::to_string(&c).unwrap()).unwrap();
        assert!(latest_cached(&home).is_none(), "过期缓存应视为无检测结果");
        // 损坏缓存 → None（不 panic）
        std::fs::write(cache_path(&home), "not json").unwrap();
        assert!(latest_cached(&home).is_none());
    }

    /// 极简 HTTP mock：按完整路径返回预置响应。
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

    /// 构造完整 mock 路由（release JSON + 资产 + checksums.txt）。
    /// `tag` 为发布 tag；返回 (routes, archive)。
    fn mock_routes(tag: &str, archive: Vec<u8>) -> (Vec<(&'static str, Vec<u8>)>, Vec<u8>) {
        let release = format!(r#"{{"tag_name":"{tag}","name":"bingo {tag}","assets":[]}}"#,);
        let asset = current_asset_name().expect("当前平台在发布矩阵内");
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
            .expect("mock 服务器应成功走完全流程");
        match outcome {
            UpdateOutcome::Updated { version, exe } => {
                assert_eq!(version.parts(), &[99, 0, 0]);
                assert_eq!(std::fs::read(&exe).unwrap(), bin);
            }
            other => panic!("期望 Updated，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_check_only_skips_download() {
        let (routes, _) = mock_routes("v99.0.0", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let client = reqwest::Client::new();
        let outcome = perform_update(&client, true, &base, &base, None)
            .await
            .expect("--check 只解析 release，不应失败");
        match outcome {
            UpdateOutcome::Available { version, .. } => {
                assert_eq!(version.parts(), &[99, 0, 0]);
            }
            other => panic!("期望 Available，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_latest_when_version_not_newer() {
        let (routes, _) = mock_routes("v0.0.1", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let client = reqwest::Client::new();
        let outcome = perform_update(&client, false, &base, &base, None)
            .await
            .expect("旧版本 tag → Latest 不应失败");
        match outcome {
            UpdateOutcome::Latest { current } => {
                assert_eq!(current, Version::from_pkg());
            }
            other => panic!("期望 Latest，得到 {other:?}"),
        }
    }

    #[tokio::test]
    async fn perform_update_checksum_mismatch_aborts() {
        let archive = sample_tar_gz(b"x");
        let release = r#"{"tag_name":"v99.0.0","name":"bingo v99.0.0","assets":[]}"#.to_string();
        let asset = current_asset_name().unwrap();
        // checksums.txt 给错误哈希 → 校验失败，不得替换可执行文件
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
            .expect_err("校验失败必须报错");
        assert!(matches!(err, UpdateError::ChecksumMismatch { .. }));
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            b"keep-me",
            "失败不得触碰现有二进制"
        );
        assert_eq!(err.error_code(), "CHECKSUM_MISMATCH");
    }

    #[tokio::test]
    async fn perform_update_http_error_propagates() {
        // 空路由：所有路径返回 404 → release 拉取报 Http{404}
        let base = serve(vec![]).await;
        let client = reqwest::Client::new();
        let err = perform_update(&client, false, &base, &base, None)
            .await
            .expect_err("404 应报 Http 错误");
        assert!(matches!(err, UpdateError::Http { status: 404 }));
        assert_eq!(err.error_code(), "OFFLINE");
    }

    #[tokio::test]
    async fn fetch_and_cache_writes_cache_and_silent_failure() {
        // 成功路径：写缓存（api_base 注入 mock，不打真实 GitHub）
        let (routes, _) = mock_routes("v3.1.4", sample_tar_gz(b"x"));
        let base = serve(routes).await;
        let home = tmp_home();
        let client = reqwest::Client::new();
        let got = fetch_and_cache(&client, &home, &base).await;
        assert!(got.is_some());
        assert_eq!(latest_cached(&home).unwrap().parts(), &[3, 1, 4]);

        // 失败路径：404 → 静默 None（无缓存写入，不 panic）
        let dead = serve(vec![]).await;
        let home2 = tmp_home();
        assert!(fetch_and_cache(&client, &home2, &dead).await.is_none());
        assert!(latest_cached(&home2).is_none());
    }
}
