//! The archives the release line publishes, by target triple.
//!
//! `.github/workflows/release.yml` builds four of them and names each
//! `bingo-<target>` with the extension its runner packs: a `tar.gz`
//! everywhere but Windows, where `Compress-Archive` writes a zip.

/// The one file every release carries beside the archives.
pub const CHECKSUMS: &str = "checksums.txt";

/// The four targets the release matrix builds. A build for anything else is a
/// build from source, and this crate says nothing to it.
pub const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// The archive a release attaches for `target`.
pub fn name(target: &str) -> String {
    match target.contains("windows") {
        true => format!("bingo-{target}.zip"),
        false => format!("bingo-{target}.tar.gz"),
    }
}

/// The triple this binary was built for, when the release line ships one —
/// and nothing at all otherwise, so a musl or a riscv build is never handed
/// an archive that would not run.
pub fn target() -> Option<&'static str> {
    if cfg!(target_env = "musl") {
        return None;
    }
    match (
        std::env::consts::ARCH,
        std::env::consts::OS,
        std::env::consts::FAMILY,
    ) {
        ("x86_64", "linux", _) => Some("x86_64-unknown-linux-gnu"),
        ("x86_64", "windows", _) => Some("x86_64-pc-windows-msvc"),
        ("aarch64", "macos", _) => Some("aarch64-apple-darwin"),
        ("x86_64", "macos", _) => Some("x86_64-apple-darwin"),
        _ => None,
    }
}

/// What the binary inside an archive is called on this platform.
pub fn binary() -> String {
    format!("bingo{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four names `release.yml`'s matrix uploads, spelled out here so a
    /// change to either side has to change both.
    #[test]
    fn the_four_names_are_the_ones_the_release_publishes() {
        assert_eq!(
            TARGETS.map(name).to_vec(),
            vec![
                "bingo-x86_64-unknown-linux-gnu.tar.gz",
                "bingo-x86_64-pc-windows-msvc.zip",
                "bingo-aarch64-apple-darwin.tar.gz",
                "bingo-x86_64-apple-darwin.tar.gz",
            ]
        );
    }

    #[test]
    fn this_build_is_one_of_them_or_none_of_them() {
        if let Some(triple) = target() {
            assert!(TARGETS.contains(&triple), "{triple}");
        }
    }

    #[test]
    fn the_binary_inside_carries_the_platforms_own_suffix() {
        let name = binary();
        assert!(name.starts_with("bingo"), "{name}");
        assert_eq!(
            name.trim_start_matches("bingo"),
            std::env::consts::EXE_SUFFIX
        );
    }
}
