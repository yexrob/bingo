//! Where a picture comes from.
//!
//! An `@word` in a composed line, a `--image` argument: either a path on this
//! machine or a URL this machine fetches (ADR-0041 §3). Which of the two a
//! word is, is a question about the word alone — so it is answered here, by a
//! pure function, and the reading is [`crate::load`]'s.

use std::fmt;
use std::path::{Path, PathBuf};

/// The two schemes a picture may arrive over. Anything else — a `file:`, a
/// `data:`, a bare word — is a path, and a path that does not exist says so.
const SCHEMES: [&str; 2] = ["http://", "https://"];

/// One picture's whereabouts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Path(PathBuf),
    Url(String),
}

impl Source {
    /// What `word` names, with a relative path taken from `cwd` — whose
    /// directory a path is in is the caller's to know, never this crate's.
    pub fn parse(word: &str, cwd: &Path) -> Source {
        match is_url(word) {
            true => Source::Url(word.to_owned()),
            false => Source::Path(cwd.join(word)),
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Path(path) => write!(f, "{}", path.display()),
            Source::Url(url) => f.write_str(url),
        }
    }
}

/// Whether a word could name a picture at all, which is what makes an `@word`
/// an attachment rather than prose. A URL is one whatever its path says — the
/// bytes are what decide, and a URL often ends in no name at all; a path is
/// one when its extension is a format the decoder knows by name.
pub fn names_a_picture(word: &str) -> bool {
    is_url(word) || has_a_pictures_extension(word)
}

fn is_url(word: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        word.get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

fn has_a_pictures_extension(word: &str) -> bool {
    Path::new(word)
        .extension()
        .is_some_and(|ext| image::ImageFormat::from_extension(ext).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_with_a_scheme_is_a_url_and_everything_else_is_a_path() {
        let cwd = Path::new("/work");
        assert_eq!(
            Source::parse("https://x/y.jpg", cwd),
            Source::Url("https://x/y.jpg".into())
        );
        assert_eq!(
            Source::parse("http://x/y", cwd),
            Source::Url("http://x/y".into())
        );
        assert_eq!(
            Source::parse("shot.png", cwd),
            Source::Path(PathBuf::from("/work/shot.png"))
        );
    }

    #[test]
    fn an_absolute_path_is_itself_and_a_relative_one_is_under_the_directory() {
        let cwd = Path::new("/work");
        assert_eq!(
            Source::parse("/tmp/shot.png", cwd),
            Source::Path(PathBuf::from("/tmp/shot.png"))
        );
        assert_eq!(
            Source::parse("sub/shot.png", cwd),
            Source::Path(PathBuf::from("/work/sub/shot.png"))
        );
    }

    /// A scheme is case-insensitive, and a word that only looks like one is
    /// still a path — nothing here reaches the network by accident.
    #[test]
    fn the_scheme_is_read_case_insensitively_and_nothing_near_it_counts() {
        let cwd = Path::new("/work");
        assert_eq!(
            Source::parse("HTTPS://x/y.png", cwd),
            Source::Url("HTTPS://x/y.png".into())
        );
        assert!(matches!(
            Source::parse("httpsomething.png", cwd),
            Source::Path(_)
        ));
        assert!(matches!(
            Source::parse("ftp://x/y.png", cwd),
            Source::Path(_)
        ));
        assert!(matches!(Source::parse("http:/x.png", cwd), Source::Path(_)));
    }

    /// A word shorter than a scheme, and a word whose first bytes are not a
    /// character boundary: neither may panic on the slice.
    #[test]
    fn a_short_or_wide_word_is_a_path() {
        let cwd = Path::new("/work");
        assert!(matches!(Source::parse("h", cwd), Source::Path(_)));
        assert!(matches!(Source::parse("", cwd), Source::Path(_)));
        assert!(matches!(Source::parse("图片.png", cwd), Source::Path(_)));
    }

    #[test]
    fn every_extension_a_decoder_knows_names_a_picture_and_a_url_always_does() {
        for word in [
            "shot.png",
            "shot.JPEG",
            "a/b.gif",
            "x.webp",
            "x.bmp",
            "x.tiff",
            "x.ico",
            "x.qoi",
            "x.tga",
            "x.ppm",
        ] {
            assert!(names_a_picture(word), "{word}");
        }
        assert!(names_a_picture("https://x/y"), "a URL names no type");
        assert!(names_a_picture("http://x/page"));
    }

    #[test]
    fn prose_and_a_source_file_name_no_picture() {
        for word in ["src/lib.rs", "notes", "Cargo.toml", "shot.png.txt"] {
            assert!(!names_a_picture(word), "{word}");
        }
    }
}
