//! Whether one release is newer than another.
//!
//! The release line tags `vX.Y.Z` and `check_release_version.py` holds the
//! tag to the workspace's version, so three numbers are the whole vocabulary
//! a comparison needs. A pre-release suffix is read but never guessed about:
//! it sorts before the release of the same numbers, as semver says, and two
//! pre-releases of the same numbers are not ranked at all.

/// A version as a tag spells it. Build metadata (`+…`) is dropped: semver
/// says it takes no part in ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Version<'a> {
    numbers: [u64; 3],
    pre: Option<&'a str>,
}

/// Whether `latest` is worth telling someone running `current` about.
///
/// Anything unreadable on either side is `false`: a check that cannot make
/// sense of an answer says nothing at all.
pub fn newer(current: &str, latest: &str) -> bool {
    match (parse(current), parse(latest)) {
        (Some(current), Some(latest)) => ahead(current, latest),
        _ => false,
    }
}

/// Whether `latest` stands above `current`.
fn ahead(current: Version<'_>, latest: Version<'_>) -> bool {
    if latest.numbers != current.numbers {
        return latest.numbers > current.numbers;
    }
    // The same numbers: only leaving a pre-release for its own release is a
    // step forward. Two pre-releases of one version are not ordered here.
    latest.pre.is_none() && current.pre.is_some()
}

/// `v0.5.0`, `0.5.0`, `0.5.0-rc.1`, `0.5.0+build` — or nothing, for anything
/// that is not three numbers.
fn parse(text: &str) -> Option<Version<'_>> {
    let text = text.trim().strip_prefix('v').unwrap_or(text.trim());
    let text = text.split('+').next()?;
    let (core, pre) = match text.split_once('-') {
        Some((core, pre)) if !pre.is_empty() => (core, Some(pre)),
        Some(_) => return None,
        None => (text, None),
    };
    let mut numbers = [0u64; 3];
    let mut parts = core.split('.');
    for slot in &mut numbers {
        *slot = parts.next()?.parse().ok()?;
    }
    match parts.next() {
        Some(_) => None,
        None => Some(Version { numbers, pre }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_higher_number_anywhere_is_newer() {
        assert!(newer("0.4.2", "0.4.3"));
        assert!(newer("0.4.2", "0.5.0"));
        assert!(newer("0.4.2", "1.0.0"));
        // Numbers, not text: 10 is above 9 and above 4.
        assert!(newer("0.9.0", "0.10.0"));
        assert!(newer("0.4.9", "0.4.10"));
    }

    #[test]
    fn the_same_version_and_an_older_one_are_not_newer() {
        assert!(!newer("0.4.2", "0.4.2"));
        assert!(!newer("0.4.2", "0.4.1"));
        assert!(!newer("0.5.0", "0.4.9"));
        assert!(!newer("1.0.0", "0.99.99"));
    }

    #[test]
    fn the_tags_leading_v_is_read_on_either_side() {
        assert!(newer("0.4.2", "v0.5.0"));
        assert!(newer("v0.4.2", "0.5.0"));
        assert!(!newer("v0.5.0", "v0.5.0"));
    }

    #[test]
    fn a_pre_release_is_behind_the_release_of_its_own_numbers() {
        assert!(newer("0.5.0-rc.1", "0.5.0"));
        assert!(!newer("0.5.0", "0.5.0-rc.1"));
        assert!(
            !newer("0.5.0-rc.1", "0.5.0-rc.2"),
            "two pre-releases are not ranked here"
        );
        assert!(newer("0.5.0-rc.1", "0.5.1"), "the numbers still decide");
    }

    #[test]
    fn build_metadata_takes_no_part() {
        assert!(!newer("0.4.2", "0.4.2+abc"));
        assert!(newer("0.4.2+abc", "0.4.3"));
    }

    #[test]
    fn nothing_unreadable_is_ever_newer() {
        for text in ["", "latest", "0.4", "0.4.2.1", "0.4.x", "v", "0.4.2-"] {
            assert!(!newer("0.4.2", text), "{text} read as a version");
            assert!(!newer(text, "0.5.0"), "{text} read as a version");
        }
    }
}
