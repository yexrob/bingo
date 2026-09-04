//! `checksums.txt`, as `sha256sum *` writes it and the release attaches it:
//! one line per asset, the digest in hex and the file name after it.

use crate::sha256;

/// The digest the file `name` should hash to, or nothing when the list does
/// not mention it.
pub fn expected(text: &str, name: &str) -> Option<[u8; 32]> {
    text.lines()
        .filter_map(line)
        .find_map(|(digest, listed)| (listed == name).then_some(digest))
}

/// Whether these bytes are the file the list is about.
pub fn matches(bytes: &[u8], expected: [u8; 32]) -> bool {
    sha256::digest(bytes) == expected
}

/// One line: sixty-four hex digits, then the name. `sha256sum` marks a file it
/// read as binary with a `*` before the name; the name is the whole rest of
/// the line, so one with a space in it still reads.
fn line(text: &str) -> Option<([u8; 32], &str)> {
    let text = text.trim_end_matches(['\r', '\n']);
    let (digest, name) = text.split_once(char::is_whitespace)?;
    let name = name
        .trim_start()
        .strip_prefix('*')
        .unwrap_or(name.trim_start());
    match name.is_empty() {
        true => None,
        false => Some((bytes(digest)?, name)),
    }
}

/// Sixty-four hex digits as thirty-two bytes.
fn bytes(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        let text = std::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the release job's `sha256sum *` leaves behind, shape for shape.
    const LIST: &str = "\
0000000000000000000000000000000000000000000000000000000000000001  bingo-aarch64-apple-darwin.tar.gz
0000000000000000000000000000000000000000000000000000000000000002  bingo-x86_64-apple-darwin.tar.gz
0000000000000000000000000000000000000000000000000000000000000003  bingo-x86_64-pc-windows-msvc.zip
0000000000000000000000000000000000000000000000000000000000000004  bingo-x86_64-unknown-linux-gnu.tar.gz
";

    #[test]
    fn every_asset_in_the_list_has_its_own_digest() {
        let mut last = [0u8; 32];
        last[31] = 3;
        assert_eq!(
            expected(LIST, "bingo-x86_64-pc-windows-msvc.zip"),
            Some(last)
        );
        assert!(expected(LIST, "bingo-aarch64-apple-darwin.tar.gz").is_some());
        assert!(expected(LIST, "bingo-x86_64-unknown-linux-gnu.tar.gz").is_some());
    }

    #[test]
    fn a_name_the_list_does_not_carry_has_no_digest() {
        assert_eq!(
            expected(LIST, "bingo-riscv64-unknown-linux-gnu.tar.gz"),
            None
        );
        assert_eq!(expected("", "bingo.tar.gz"), None);
    }

    #[test]
    fn a_binary_mark_and_a_single_space_read_the_same() {
        let digest = expected(
            "0000000000000000000000000000000000000000000000000000000000000005 *bingo.tar.gz",
            "bingo.tar.gz",
        );
        assert!(
            digest.is_some(),
            "sha256sum's binary mark is not part of the name"
        );
    }

    #[test]
    fn a_line_that_is_not_a_digest_and_a_name_is_skipped() {
        let broken = "not-a-digest bingo.tar.gz\nzz00000000000000000000000000000000000000000000000000000000000001  bingo.tar.gz\n";
        assert_eq!(expected(broken, "bingo.tar.gz"), None);
    }

    #[test]
    fn the_digest_the_list_carries_is_the_one_the_bytes_hash_to() {
        let bytes = b"a release archive";
        let list = format!("{}  bingo.tar.gz\n", sha256::hex(&sha256::digest(bytes)));
        let digest = expected(&list, "bingo.tar.gz").expect("the name is listed");
        assert!(matches(bytes, digest));
        assert!(!matches(b"something else", digest));
    }
}
