//! `bingo update`: this binary becomes the newest release, or says why it
//! could not (M63, ADR-0043).
//!
//! Stdout carries the two versions and, when something happened, one line
//! saying what; every step of the way is on stderr, as every diagnostic is.
//! Nothing is ever run with elevated rights: a directory this process may not
//! write is a failure with `cargo install` in it, not a password prompt.

use bingo_sdk::{Env, ErrorCode, KernelError};
use bingo_update::{Release, api, asset, checksums, install};

/// This build's version — the tag a release of it carries.
const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The whole command. `check` reports and stops.
pub async fn run(env: &Env, check: bool) -> Result<i32, KernelError> {
    let client = client(api::TIMEOUT)?;
    let release = latest(&client).await?;
    println!("current: {CURRENT}");
    println!("latest:  {}", release.version);
    if check || !bingo_update::version::newer(CURRENT, &release.version) {
        return Ok(0);
    }
    become_it(env, &release).await?;
    println!("updated to {}", release.version);
    Ok(0)
}

/// Fetch, verify, unpack, and take the running binary's place.
async fn become_it(env: &Env, release: &Release) -> Result<(), KernelError> {
    let blame = |e: install::InstallError| failed(&e, &release.version);
    let target = install::running().map_err(blame)?;
    install::sweep(&target);
    let name = archive_name()?;
    let (archive, digest) = fetched(release, &name).await?;
    if !checksums::matches(&archive, digest) {
        return Err(invalid(format!("{name} is not what checksums.txt says")));
    }
    let staging = install::staging(&env.data_dir);
    let outcome = unpacked(&staging, &name, &archive, &target).map_err(blame);
    let _ = std::fs::remove_dir_all(&staging);
    outcome
}

/// The archive on disk, unpacked, and put where the running binary is. The
/// staging directory is the caller's to remove, whichever way this goes.
fn unpacked(
    staging: &std::path::Path,
    name: &str,
    archive: &[u8],
    target: &std::path::Path,
) -> Result<(), install::InstallError> {
    let path = staging.join(name);
    written(staging, || std::fs::create_dir_all(staging))?;
    written(&path, || std::fs::write(&path, archive))?;
    eprintln!("Unpacking {name}…");
    let new = install::unpack(&path, &staging.join("unpacked"))?;
    let staged = install::stage(&new, target)?;
    eprintln!("Replacing {}…", target.display());
    install::swap(&staged, target)
}

/// A write of this command's own, reported the way the install half reports
/// its writes — one shape of "this directory would not have it".
fn written(
    path: &std::path::Path,
    write: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), install::InstallError> {
    write().map_err(|source| install::InstallError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// The archive this build's target is published as, and the digest the
/// release's own `checksums.txt` gives for it.
async fn fetched(release: &Release, name: &str) -> Result<(Vec<u8>, [u8; 32]), KernelError> {
    let client = client(api::DOWNLOAD_TIMEOUT)?;
    let list = asset_url(release, asset::CHECKSUMS)?;
    let list = text(&client, &list).await?;
    let digest = checksums::expected(&list, name)
        .ok_or_else(|| invalid(format!("checksums.txt does not list {name}")))?;
    eprintln!("Downloading {name}…");
    let archive = bytes(&client, &asset_url(release, name)?).await?;
    Ok((archive, digest))
}

/// The archive for this build's target, or a build the release line does not
/// publish for — a musl or a riscv one, which updates the way it was made.
fn archive_name() -> Result<String, KernelError> {
    let target = asset::target().ok_or_else(|| {
        invalid(format!(
            "this build ({} {}) is not one the release publishes; build it again from source",
            std::env::consts::ARCH,
            std::env::consts::OS,
        ))
    })?;
    Ok(asset::name(target))
}

fn asset_url(release: &Release, name: &str) -> Result<String, KernelError> {
    release
        .asset(name)
        .map(|asset| asset.url.clone())
        .ok_or_else(|| invalid(format!("release {} carries no {name}", release.version)))
}

async fn latest(client: &reqwest::Client) -> Result<Release, KernelError> {
    let answer = text(client, &api::latest_url()).await?;
    bingo_update::release::latest(&answer).map_err(|e| invalid(e.to_string()))
}

fn client(timeout: std::time::Duration) -> Result<reqwest::Client, KernelError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(api::user_agent(CURRENT))
        .build()
        .map_err(|e| internal(e.to_string()))
}

async fn text(client: &reqwest::Client, url: &str) -> Result<String, KernelError> {
    answered(client, url)
        .await?
        .text()
        .await
        .map_err(|e| unreachable(url, &e))
}

async fn bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, KernelError> {
    answered(client, url)
        .await
        .map(|answer| answer.bytes())?
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|e| unreachable(url, &e))
}

async fn answered(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, KernelError> {
    client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| unreachable(url, &e))
}

fn unreachable(url: &str, e: &reqwest::Error) -> KernelError {
    KernelError::new(ErrorCode::Internal, format!("{url}: {e}"))
}

/// A failure of the install half. A directory this process may not write is
/// the one with an answer of its own, and the answer is never `sudo`.
fn failed(e: &install::InstallError, version: &str) -> KernelError {
    let mut message = e.to_string();
    if e.is_permission() {
        message.push_str("\n  build it there instead: ");
        message.push_str(&api::from_source(version));
    }
    KernelError::new(ErrorCode::Internal, message)
}

fn invalid(message: String) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

fn internal(message: String) -> KernelError {
    KernelError::new(ErrorCode::Internal, message)
}
