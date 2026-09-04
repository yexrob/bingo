//! Where a check asks, and how it announces itself.
//!
//! One address, one header, one clock — kept here so the surface that asks at
//! start and the command that installs ask the same thing the same way.

use std::time::Duration;

/// The repository the release line publishes to.
pub const REPO: &str = "yexrob/bingo";

/// The API a release is read from. `BINGO_UPDATE_API` replaces the origin,
/// which is how a test serves a release of its own on the loopback; nothing
/// else reads it, and a person who sets it is pointing their own binary
/// somewhere on purpose.
pub const ORIGIN_ENV: &str = "BINGO_UPDATE_API";

const ORIGIN: &str = "https://api.github.com";

/// What one request may take, from the connection to the last byte. A start
/// that has to wait longer than this has waited too long already.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// What a download may take: an archive is twenty megabytes and a person is
/// watching it, which is a different clock from a start-up check's.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// The newest release that is neither a draft nor a pre-release.
pub fn latest_url() -> String {
    format!("{}/repos/{REPO}/releases/latest", origin())
}

/// How this build names itself to the API, which asks every caller to.
pub fn user_agent(current: &str) -> String {
    format!("bingo/{current}")
}

/// The way round a permission failure: the same version, built from source.
pub fn from_source(version: &str) -> String {
    format!("cargo install --git https://github.com/{REPO} --tag v{version} bingo")
}

fn origin() -> String {
    std::env::var(ORIGIN_ENV)
        .ok()
        .filter(|origin| !origin.trim().is_empty())
        .unwrap_or_else(|| ORIGIN.to_string())
        .trim_end_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_is_the_repositorys_own_latest_release() {
        // The environment is process-wide, so this reads the default only
        // when nothing else in the process has set the override.
        if std::env::var(ORIGIN_ENV).is_err() {
            assert_eq!(
                latest_url(),
                "https://api.github.com/repos/yexrob/bingo/releases/latest"
            );
        }
    }

    #[test]
    fn a_build_names_itself_and_the_way_round_names_a_tag() {
        assert_eq!(user_agent("0.4.2"), "bingo/0.4.2");
        assert!(from_source("0.5.0").ends_with("--tag v0.5.0 bingo"));
    }
}
