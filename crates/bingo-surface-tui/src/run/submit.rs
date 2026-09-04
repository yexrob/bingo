//! A line on its way out of the composer.
//!
//! What a person typed goes to the mailbox of the session it was typed at, and
//! the pictures it named go with it. An `@word` is a path off this machine's
//! disk or a URL this machine fetches (ADR-0041 §3): both are read on a task of
//! their own and mailed back, so no key press waits on a web server and a line
//! that could not be sent is handed back to the composer whole.

use bingo_sdk::{Image, Input, Level, Origin, SessionHandle};

use super::{Reply, Run};
use crate::{complete, history};

/// A submitted line waiting on the pictures it named. It carries the mailbox
/// it was typed at, so a person who switches session while a picture is in
/// flight still sends it where they wrote it.
pub(super) struct Mentioned {
    handle: SessionHandle,
    text: String,
    origin: Origin,
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
            } => self.submit_text(handle, text, images, origin),
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
    ) {
        let mentions = complete::attachments(&text);
        if mentions.is_empty() {
            return self.send_text(handle, text, images, origin);
        }
        let cwd = std::path::PathBuf::from(&self.session.tree.root().summary.cwd);
        self.spawn(async move {
            let images = read_mentions(mentions, &cwd, images).await;
            Ok(Reply::Mentioned(Box::new(Mentioned {
                handle,
                text,
                origin,
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
            images,
        } = waiting;
        match images {
            Ok(images) => self.send_text(handle, text, images, origin),
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
            },
        );
    }
}

/// `images` with the pictures `mentions` name read in after them, in the
/// order they were written; the first that does not read is what comes back
/// instead, in the words the notice will carry. Off the loop's thread: a
/// path is read from disk and a URL is fetched by this machine (ADR-0041 §3).
async fn read_mentions(
    mentions: Vec<String>,
    cwd: &std::path::Path,
    mut images: Vec<Image>,
) -> Result<Vec<Image>, String> {
    for word in mentions {
        // A mentioned picture is journaled, so the session itself is where it
        // is kept and the cache would be a second copy of it (M61).
        match bingo_pictures::load(&bingo_pictures::Source::parse(&word, cwd), None).await {
            Ok(image) => images.push(image),
            Err(error) => return Err(format!("{word}: {error}")),
        }
    }
    Ok(images)
}
