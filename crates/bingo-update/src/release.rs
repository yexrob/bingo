//! The release the API answers with, and the archives hanging off it.
//!
//! `/repos/<owner>/<repo>/releases/latest` answers the newest release that is
//! neither a draft nor a pre-release, which is exactly the question "is there
//! a newer one" wants asked: `release.yml` marks a tag that is not on `main`
//! as a pre-release, so a preflight cut never reaches anybody.

use serde::Deserialize;

/// A published release: what it is called, and what it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// The tag without its `v`, so it compares against a crate version.
    pub version: String,
    pub assets: Vec<Asset>,
}

/// One file attached to a release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("the release answer is not JSON: {0}")]
    NotJson(String),
    #[error("the release answer names no tag")]
    NoTag,
}

impl Release {
    /// The archive by name, when the release carries it.
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|asset| asset.name == name)
    }
}

/// Read one release out of the API's answer.
pub fn latest(json: &str) -> Result<Release, ReleaseError> {
    let answer: Answer =
        serde_json::from_str(json).map_err(|e| ReleaseError::NotJson(e.to_string()))?;
    let tag = answer.tag_name.ok_or(ReleaseError::NoTag)?;
    Ok(Release {
        version: tag.trim().trim_start_matches('v').to_string(),
        assets: answer
            .assets
            .into_iter()
            .map(|asset| Asset {
                name: asset.name,
                url: asset.browser_download_url,
            })
            .collect(),
    })
}

/// The fields of the answer this crate reads, and no others: everything else
/// GitHub sends is left where it is.
#[derive(Deserialize)]
struct Answer {
    tag_name: Option<String>,
    #[serde(default)]
    assets: Vec<AnswerAsset>,
}

#[derive(Deserialize)]
struct AnswerAsset {
    name: String,
    browser_download_url: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A release answer under `fixtures/`, shaped as the API's is.
    fn fixture() -> String {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/releases-latest.json");
        std::fs::read_to_string(path).expect("the fixture is there")
    }

    #[test]
    fn the_tag_is_the_version_without_its_v() {
        let release = latest(&fixture()).expect("a release");
        assert_eq!(release.version, "0.4.2");
    }

    #[test]
    fn every_archive_the_release_matrix_uploads_is_an_asset() {
        let release = latest(&fixture()).expect("a release");
        for target in crate::asset::TARGETS {
            let name = crate::asset::name(target);
            let asset = release.asset(&name).unwrap_or_else(|| panic!("{name}"));
            assert!(asset.url.ends_with(&name), "{}", asset.url);
        }
        assert!(release.asset(crate::asset::CHECKSUMS).is_some());
    }

    #[test]
    fn an_asset_the_release_does_not_carry_is_none() {
        let release = latest(&fixture()).expect("a release");
        assert!(release.asset("bingo-riscv64.tar.gz").is_none());
    }

    #[test]
    fn an_answer_that_is_not_a_release_is_an_error_and_not_a_panic() {
        let rate_limited = r#"{"message":"API rate limit exceeded","documentation_url":"…"}"#;
        assert!(matches!(latest(rate_limited), Err(ReleaseError::NoTag)));
        assert!(matches!(latest("<html>"), Err(ReleaseError::NotJson(_))));
        assert!(matches!(latest(""), Err(ReleaseError::NotJson(_))));
    }

    #[test]
    fn a_release_with_no_assets_still_reads() {
        let release = latest(r#"{"tag_name":"v9.9.9"}"#).expect("a release");
        assert_eq!(release.version, "9.9.9");
        assert!(release.assets.is_empty());
    }
}
