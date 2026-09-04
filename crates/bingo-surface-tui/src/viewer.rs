//! Handing a picture to whatever this system opens pictures with.
//!
//! A click on a drawn picture opens it (design §7: the mouse is welcome). What
//! is handed over is always a **file on this machine**, never an address: a
//! picture is a picture, and the viewer is what shows one — a browser sent to
//! fetch it again would be a second reading of bytes this surface already has.
//!
//! So there are two outcomes and no third. A destination an answer wrote that
//! names a file opens that file, where it is. Everything else is bytes in hand
//! — a tool's answer, a paste, and a fetched address, whose picture the memo is
//! already holding ([`linked::Linked`]) — and they are written out under the
//! number the picture is already known by ([`Source::id`]) and that file is
//! opened. One file per picture, overwritten, so a picture opened a hundred
//! times leaves one behind.
//!
//! The opener is the one this workspace has (`bingo-loopback`, ADR-0042 §1):
//! every platform's spelling of "open this", and `BINGO_NO_BROWSER` to keep a
//! run from taking the screen.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bingo_sdk::Image;

use crate::graphics::linked;
use crate::graphics::picture::Source;

/// Where the bytes that have no name of their own are written, under the data
/// directory.
pub const DIR: &str = "pictures";

/// Who opens it: this system's own viewer, or a test's recorder — the one seam
/// a test reaches through, as `ShowPage`'s opener is (ADR-0042 §4).
pub type Opener = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The system's own, which is what a run uses.
pub fn system() -> Opener {
    Arc::new(bingo_loopback::browser::open)
}

/// The places this machine keeps pictures: a session's own directory and home
/// for a path an answer wrote, the data directory for bytes that have no path.
#[derive(Clone, Copy)]
pub struct Where<'a> {
    pub cwd: &'a Path,
    pub home: Option<&'a str>,
    pub data_dir: &'a Path,
}

/// Why a picture could not be turned into a thing to open at all.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// It is no longer where it was drawn from: a rewind dropped the item, or
    /// the draft that held it is gone.
    #[error("the picture is no longer there")]
    Gone,
    #[error("{0}")]
    Undecodable(#[from] bingo_pictures::PictureError),
    #[error("{0}")]
    Unwritable(#[from] std::io::Error),
}

/// Open a picture. `Ok` is the word the system was handed and took; `Err` is
/// what a person is told instead — the word nothing on this machine would
/// open, or why there was no word to hand over.
pub fn open(
    opener: &Opener,
    source: &Source,
    image: Option<&Image>,
    at: Where<'_>,
) -> Result<String, String> {
    let word = word(source, image, at).map_err(|why| why.to_string())?;
    match opener(&word) {
        true => Ok(word),
        false => Err(format!("nothing on this machine opened {word}")),
    }
}

/// The file the opener is handed for a picture: the path an answer wrote, or
/// the one this surface wrote its bytes to.
pub fn word(source: &Source, image: Option<&Image>, at: Where<'_>) -> Result<String, Error> {
    if let Some(path) = path_named(source, at) {
        return Ok(path);
    }
    let image = image.ok_or(Error::Gone)?;
    let path = written(source.id(), image, at.data_dir)?;
    Ok(path.to_string_lossy().into_owned())
}

/// The path a picture already has on this machine: a destination an answer
/// wrote that names a file, made whole from the session's directory by the
/// reading the picture was read in by ([`linked::source`]), so what is opened
/// is what was drawn.
///
/// An address names no file. Its bytes were fetched and are in hand, so they go
/// the way every other picture's bytes go — the viewer is handed a file, and
/// nothing is fetched twice.
fn path_named(source: &Source, at: Where<'_>) -> Option<String> {
    let Source::Linked { dest } = source else {
        return None;
    };
    match linked::source(dest, at.cwd, at.home) {
        bingo_pictures::Source::Path(path) => Some(path.to_string_lossy().into_owned()),
        bingo_pictures::Source::Url(_) => None,
    }
}

/// The file a picture's bytes are opened at: `pictures/<id>.png` under the data
/// directory, as the one format every decoder in this tree agrees on.
fn written(id: u32, image: &Image, data_dir: &Path) -> Result<PathBuf, Error> {
    let png = bingo_pictures::to_png(image)?;
    let dir = data_dir.join(DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id:06x}.png"));
    std::fs::write(&path, &png.bytes)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::{png, unreadable};
    use std::sync::Mutex;

    /// An opener that takes everything and remembers what it was handed.
    fn recording() -> (Opener, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        let opener: Opener = Arc::new(move |word: &str| {
            kept.lock().expect("the record").push(word.to_string());
            true
        });
        (opener, seen)
    }

    fn journal() -> Source {
        Source::Journal {
            item: bingo_sdk::ItemId::from_raw("itm_1"),
            part: 0,
        }
    }

    /// A picture an answer's words named as a file is opened where it is: a
    /// relative path from the session's own directory, `~` for home, an
    /// absolute one as it stands — the reading the picture was read in by, and
    /// no other. No bytes are wanted for it and none are written.
    #[test]
    fn a_path_the_words_named_is_opened_where_it_was_written() {
        let at = Where {
            cwd: Path::new("/work"),
            home: Some("/home/me"),
            data_dir: Path::new("/nowhere"),
        };
        let word = |dest: &str| {
            word(
                &Source::Linked {
                    dest: dest.to_string(),
                },
                None,
                at,
            )
            .expect("a file to open")
        };
        assert_eq!(word("docs/x.png"), "/work/docs/x.png");
        assert_eq!(word("~/shots/x.png"), "/home/me/shots/x.png");
        assert_eq!(word("/etc/x.png"), "/etc/x.png");
    }

    /// A picture the words named by *address* is never an address to open: the
    /// bytes behind it are in hand, so they are written out under the picture's
    /// own number and the viewer is handed that file — the same file a tool's
    /// answer or a paste would be opened at.
    #[test]
    fn an_address_is_opened_as_the_file_its_bytes_were_written_to() {
        let dir = tempfile::tempdir().expect("a directory");
        let at = Where {
            cwd: dir.path(),
            home: None,
            data_dir: dir.path(),
        };
        let source = Source::Linked {
            dest: "https://x.dev/a.png".into(),
        };
        let image = png(4, 3);
        let word = word(&source, Some(&image), at).expect("a file to open");
        assert_eq!(
            word,
            dir.path()
                .join(DIR)
                .join(format!("{:06x}.png", source.id()))
                .to_string_lossy(),
        );
        assert!(
            std::fs::read(&word)
                .expect("the file is there")
                .starts_with(b"\x89PNG")
        );
        assert!(
            matches!(super::word(&source, None, at), Err(Error::Gone)),
            "and an address whose picture is not in hand opens nothing"
        );
    }

    /// A picture that is only bytes is written out under the number it is
    /// already known by, once: the same picture opened again overwrites its own
    /// file rather than leaving a second one.
    #[test]
    fn bytes_are_written_out_under_the_number_they_are_known_by() {
        let dir = tempfile::tempdir().expect("a directory");
        let at = Where {
            cwd: dir.path(),
            home: None,
            data_dir: dir.path(),
        };
        let image = png(4, 3);
        let word = word(&journal(), Some(&image), at).expect("a file to open");
        assert_eq!(
            word,
            dir.path()
                .join(DIR)
                .join(format!("{:06x}.png", journal().id()))
                .to_string_lossy(),
        );
        let bytes = std::fs::read(&word).expect("the file is there");
        assert!(bytes.starts_with(b"\x89PNG"), "and it is a PNG");

        let again = word;
        let word = super::word(&journal(), Some(&image), at).expect("a file to open");
        assert_eq!(word, again, "the same picture is the same file");
        assert_eq!(
            std::fs::read_dir(dir.path().join(DIR))
                .expect("the directory")
                .count(),
            1,
            "and one file, not two"
        );
    }

    /// A picture the source no longer holds, and one no decoder reads, are both
    /// a reason rather than a word — there is nothing to open.
    #[test]
    fn a_picture_that_is_gone_or_unreadable_is_no_word_at_all() {
        let dir = tempfile::tempdir().expect("a directory");
        let at = Where {
            cwd: dir.path(),
            home: None,
            data_dir: dir.path(),
        };
        assert!(matches!(word(&journal(), None, at), Err(Error::Gone),));
        assert!(matches!(
            word(&journal(), Some(&unreadable()), at),
            Err(Error::Undecodable(_)),
        ));
        assert!(
            !dir.path().join(DIR).exists(),
            "and nothing was written for it"
        );
    }

    /// The word goes to the opener, and an opener that will not take it says so
    /// with the word in the answer — what the notice a person reads carries.
    #[test]
    fn the_word_goes_to_the_opener_and_a_refusal_names_it() {
        let at = Where {
            cwd: Path::new("/work"),
            home: None,
            data_dir: Path::new("/nowhere"),
        };
        let source = Source::Linked {
            dest: "docs/x.png".into(),
        };
        let (opener, seen) = recording();
        assert_eq!(
            open(&opener, &source, None, at).as_deref(),
            Ok("/work/docs/x.png")
        );
        assert_eq!(
            seen.lock().expect("the record").as_slice(),
            ["/work/docs/x.png".to_string()]
        );

        let shut: Opener = Arc::new(|_| false);
        assert_eq!(
            open(&shut, &source, None, at),
            Err("nothing on this machine opened /work/docs/x.png".into())
        );
    }
}
