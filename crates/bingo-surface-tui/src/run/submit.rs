//! A line on its way out of the composer.
//!
//! What a person typed goes to the mailbox of the session it was typed at, and
//! the pictures it named go with it. Every one of them is a file (ADR-0052):
//! the `[image N]` tokens name the files their pastes were written to and an
//! `@word` is a path off this machine's disk or a URL this machine fetches
//! (ADR-0041 §3). All of them are read on a task of their own, in the line's
//! own order, and mailed back, so no key press waits on a disk or a web
//! server and a line that could not be sent is handed back to the composer
//! whole.

use std::path::Path;

use bingo_pictures::Source;
use bingo_sdk::{Delivery, Image, Input, Level, Origin, SessionHandle};

use super::{Reply, Run};
use crate::pictures::Held;
use crate::{complete, history};

/// A submitted line waiting on the pictures it named. It carries the mailbox
/// it was typed at, so a person who switches session while a picture is in
/// flight still sends it where they wrote it.
pub(super) struct Mentioned {
    handle: SessionHandle,
    text: String,
    origin: Origin,
    /// Whether the line steers the running turn or waits for the next one:
    /// the key that sent it decided, and a picture read on the way does not
    /// change what was pressed.
    delivery: Delivery,
    /// The composer's own pictures with the mentions read in after them, or
    /// the first mention that did not read, in the words a person is shown.
    images: Result<Vec<Image>, String>,
}

impl Run {
    pub(super) fn submit(&mut self, input: Input) {
        let Some(handle) = self.session.writer() else {
            return self.not_yet();
        };
        match input {
            Input::Text {
                text,
                images,
                origin,
                delivery,
            } => self.submit_text(handle, text, images, origin, delivery),
            action => {
                let intent = self.mint(None);
                handle.submit(intent, action);
            }
        }
    }

    /// A line goes as soon as the pictures it names are in hand. A line that
    /// names none goes now; the rest are read on their own task and come back
    /// as a reply.
    fn submit_text(
        &mut self,
        handle: SessionHandle,
        text: String,
        images: Vec<Image>,
        origin: Origin,
        delivery: Delivery,
    ) {
        let cwd = std::path::PathBuf::from(&self.session.tree.root().summary.cwd);
        let named = sources(&text, &self.ui.pictures, &cwd);
        if named.is_empty() {
            return self.send_text(handle, text, images, origin, delivery);
        }
        self.spawn(async move {
            let images = read_all(named, images).await;
            Ok(Reply::Mentioned(Box::new(Mentioned {
                handle,
                text,
                origin,
                delivery,
                images,
            })))
        });
    }

    /// The pictures are in hand: the words go with them, or the line comes
    /// back with the reason it did not — nothing is sent then, and what was
    /// typed is not lost.
    pub(super) fn mentioned(&mut self, waiting: Mentioned) {
        let Mentioned {
            handle,
            text,
            origin,
            delivery,
            images,
        } = waiting;
        match images {
            Ok(images) => self.send_text(handle, text, images, origin, delivery),
            Err(why) => {
                self.ui.notify(Level::Warn, why, std::time::Instant::now());
                self.ui.composer.set(&text);
            }
        }
    }

    /// The words, their pictures, and the line they leave in the history.
    fn send_text(
        &mut self,
        handle: SessionHandle,
        text: String,
        images: Vec<Image>,
        origin: Origin,
        delivery: Delivery,
    ) {
        history::append(&self.data_dir, &text);
        self.ui.pictures.clear();
        let intent = self.mint(Some(text.clone()));
        handle.submit(
            intent,
            Input::Text {
                text,
                images,
                origin,
                delivery,
            },
        );
    }
}

/// One picture the line names: what a person would know it by — the word
/// they typed, or the file a paste was written to — and where that is.
#[derive(Debug, PartialEq)]
struct Named {
    name: String,
    source: Source,
}

/// Where the line's pictures are, in the line's own order: the files its
/// tokens hold, then the `@word`s it mentions — the order they were journaled
/// in before there was a file behind a token, kept so a withdrawn line comes
/// back the same (M68).
fn sources(text: &str, held: &Held, cwd: &Path) -> Vec<Named> {
    let pasted = held.carried(text).into_iter().map(|path| Named {
        name: path.display().to_string(),
        source: Source::Path(path),
    });
    let mentioned = complete::attachments(text).into_iter().map(|word| Named {
        source: Source::parse(&word, cwd),
        name: word,
    });
    pasted.chain(mentioned).collect()
}

/// `images` with the pictures `named` read in after them, in order; the
/// first that does not read is what comes back instead, by the name the
/// person knows it by. Off the loop's thread: a path is read from disk and
/// a URL is fetched by this machine (ADR-0041 §3).
async fn read_all(named: Vec<Named>, mut images: Vec<Image>) -> Result<Vec<Image>, String> {
    for Named { name, source } in named {
        // A picture in the ask is journaled, so the session itself is where
        // it is kept and the cache would be a second copy of it (M61).
        match bingo_pictures::load(&source, None).await {
            Ok(image) => images.push(image),
            Err(error) => return Err(format!("{name}: {error}")),
        }
    }
    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// The line decides what goes and in what order: its tokens' files
    /// first, then the words that name a path or an address, a relative
    /// path taken from the session's own directory.
    #[test]
    fn the_lines_pictures_are_its_tokens_files_and_then_its_words() {
        let mut held = Held::default();
        held.hold("", PathBuf::from("/pasted/a.png"));
        held.hold("[image 1]", PathBuf::from("/pasted/b.png"));
        let named = sources(
            "[image 2] see @shot.png and @https://x.dev/y.jpg [image 1]",
            &held,
            Path::new("/work"),
        );
        let wheres: Vec<&Source> = named.iter().map(|one| &one.source).collect();
        assert_eq!(
            wheres,
            vec![
                &Source::Path(PathBuf::from("/pasted/b.png")),
                &Source::Path(PathBuf::from("/pasted/a.png")),
                &Source::Path(PathBuf::from("/work/shot.png")),
                &Source::Url("https://x.dev/y.jpg".into()),
            ]
        );
        let names: Vec<&str> = named.iter().map(|one| one.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "/pasted/b.png",
                "/pasted/a.png",
                "shot.png",
                "https://x.dev/y.jpg"
            ],
            "a word is known by the word, a paste by its file"
        );
        assert!(sources("plain words", &held, Path::new("/work")).is_empty());
    }

    /// What is read is what is sent, each picture knowing its file; the
    /// first that will not read names itself and nothing goes.
    #[tokio::test]
    async fn every_named_picture_is_read_in_order_and_one_missing_stops_the_line() {
        let dir = tempfile::tempdir().expect("a directory");
        let a = dir.path().join("a.png");
        std::fs::write(&a, bingo_pictures::testing::png_bytes(2, 2)).expect("written");
        let named = |path: &PathBuf| Named {
            name: path.display().to_string(),
            source: Source::Path(path.clone()),
        };
        let read = read_all(vec![named(&a)], Vec::new()).await.expect("read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].path.as_deref(), Some(a.as_path()));

        let missing = dir.path().join("gone.png");
        let refused = read_all(vec![named(&a), named(&missing)], Vec::new())
            .await
            .expect_err("refused");
        assert!(
            refused.starts_with(&missing.display().to_string()),
            "{refused}"
        );
    }
}
