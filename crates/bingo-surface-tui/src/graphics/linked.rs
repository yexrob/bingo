//! The pictures the words themselves named, read in once each.
//!
//! `![what it is](path or URL)` in an answer is a picture where this terminal
//! draws pictures (design §5), but the words are all the journal holds: the
//! bytes are on a disk or behind an address, and somebody has to go for them.
//! This is the one place that remembers what became of a destination — what is
//! on its way, what came back, and what will not be asked for again.
//!
//! Nothing here reaches a disk or a network on the draw thread. A frame
//! answers out of what is already known and draws the chip for the rest; the
//! reading is the run's, between frames, on a task of its own.
//!
//! There is no *wanted* state. What a frame wants is a fact about the frame,
//! and the block whose words named it carries it
//! ([`crate::transcript::Block`]) — one representation, on the side that knows.

use std::path::Path;

use bingo_pictures::{Cache, PictureError, Source};
use bingo_sdk::Image;

/// How many destinations one session reads in. It is a bound as much as a
/// memo: past this many the words draw their chips and nothing more is
/// fetched, so a transcript full of links can neither fill this process nor
/// spend it on a hundred files. The same count as the pictures the terminal
/// itself is asked to hold ([`super::stored::KEPT`]).
const MOST: usize = super::stored::KEPT;

/// Where one destination has got to.
#[derive(Debug)]
enum State {
    /// Asked for, and not yet answered.
    Loading,
    Loaded(Image),
    /// It will not be asked for again this session, and the chip says why.
    Failed(String),
}

/// One destination, read in.
#[derive(Debug)]
pub struct Answer {
    pub dest: String,
    /// The picture, or the few words the chip carries instead.
    pub result: Result<Image, String>,
}

/// What became of every destination this session's words named, in the order
/// they were first asked for.
#[derive(Debug, Default)]
pub struct Linked {
    kept: Vec<(String, State)>,
    answers: u64,
}

impl Linked {
    /// The picture behind a destination, where one has been read in.
    pub fn image(&self, dest: &str) -> Option<&Image> {
        match self.state(dest)? {
            State::Loaded(image) => Some(image),
            _ => None,
        }
    }

    /// Why a destination draws no picture, in the words that go after its
    /// name. Nothing at all while it is still on its way: a picture arriving
    /// is not a picture that failed.
    pub fn failure(&self, dest: &str) -> Option<&str> {
        match self.state(dest)? {
            State::Failed(why) => Some(why),
            _ => None,
        }
    }

    /// How many answers have landed. A block is drawn once and kept ever after
    /// ([`crate::blocks`]), so a picture that arrived after its block was drawn
    /// would never reach the screen unless the block could tell it had: this is
    /// what tells a rendering taken before an answer from one taken after it.
    pub fn answers(&self) -> u64 {
        self.answers
    }

    /// Take this destination for reading: `true` the first time it is asked
    /// for and never again. A picture that is not there is not there all
    /// session, and one already in hand is not fetched twice — so the words
    /// of a hundred frames cost one read.
    pub fn take(&mut self, dest: &str) -> bool {
        if self.state(dest).is_some() || self.kept.len() >= MOST {
            return false;
        }
        self.kept.push((dest.to_owned(), State::Loading));
        true
    }

    /// [`Linked::take`] over a whole frame's worth: the destinations among
    /// `wanted` that have still to be read, and where each of them points.
    /// Asking is taking, so a frame that names the same picture on every one
    /// of thirty draws a second sends nobody after it twice.
    pub fn take_all(
        &mut self,
        wanted: Vec<String>,
        cwd: &Path,
        home: Option<&str>,
    ) -> Vec<(String, Source)> {
        wanted
            .into_iter()
            .filter(|dest| self.take(dest))
            .map(|dest| {
                let source = source(&dest, cwd, home);
                (dest, source)
            })
            .collect()
    }

    /// What came back. A destination nobody took cannot be answered: the
    /// answer belongs to the read this memo started.
    pub fn answered(&mut self, answer: Answer) {
        let Some((_, state)) = self.kept.iter_mut().find(|(kept, _)| *kept == answer.dest) else {
            return;
        };
        *state = match answer.result {
            Ok(image) => State::Loaded(image),
            Err(why) => State::Failed(why),
        };
        self.answers += 1;
    }

    fn state(&self, dest: &str) -> Option<&State> {
        self.kept
            .iter()
            .find(|(kept, _)| kept == dest)
            .map(|(_, state)| state)
    }
}

/// Where a destination points, as this machine spells it: an `http(s)` address
/// this machine fetches, and everything else a path — `~` for home, and a
/// relative one in the session's own directory.
///
/// Nothing else is ever reached. A `data:`, an `ftp:` or a bare word is a path
/// that is not there, which is the answer a person sees rather than a scheme
/// this surface invented a way to follow.
pub fn source(dest: &str, cwd: &Path, home: Option<&str>) -> Source {
    Source::parse(&crate::paths::expand(dest, home), cwd)
}

/// The picture at a destination, off the loop's own thread. A fetched one is
/// kept on disk where the run has a cache, so the same address across sessions
/// is one fetch (M61).
pub async fn read(dest: String, source: Source, cache: Option<Cache>) -> Answer {
    Answer {
        result: bingo_pictures::load(&source, cache.as_ref())
            .await
            .map_err(|e| note(&e)),
        dest,
    }
}

/// Why a picture is not drawn, in the words that fit in a dim note at the end
/// of a line. The error's own `Display` is the second half of a sentence, for
/// a notice with a row to itself; this is a parenthesis after a name, so it
/// says the kind of failure and not the operating system's account of it.
fn note(error: &PictureError) -> String {
    match error {
        PictureError::Unreadable(io) if io.kind() == std::io::ErrorKind::NotFound => "not found",
        PictureError::Unreadable(_) => "not read",
        // A server that answered is named by its answer: `HTTP 404` tells a
        // person the address is wrong where `not fetched` would tell them
        // to check the network.
        PictureError::Unfetchable(error) => {
            return match error.status() {
                Some(status) => format!("HTTP {}", status.as_u16()),
                None => "not fetched".to_string(),
            };
        }
        PictureError::Refused(_) => "too large",
        _ => "not a picture",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::png;

    fn answer(dest: &str, result: Result<Image, String>) -> Answer {
        Answer {
            dest: dest.to_owned(),
            result,
        }
    }

    /// The whole of the state machine: unknown until a frame's words name it,
    /// taken once, and whatever came back is what it says ever after.
    #[test]
    fn a_destination_is_taken_once_and_keeps_the_answer_it_got() {
        let mut linked = Linked::default();
        assert_eq!(linked.image("a.png"), None, "nothing is known of it");
        assert_eq!(linked.failure("a.png"), None);
        assert_eq!(linked.answers(), 0);

        assert!(linked.take("a.png"), "the first frame takes it");
        assert!(!linked.take("a.png"), "and no frame after it does");
        assert_eq!(linked.image("a.png"), None, "still on its way");
        assert_eq!(linked.failure("a.png"), None, "and not a failure yet");

        linked.answered(answer("a.png", Ok(png(2, 3))));
        assert_eq!(linked.image("a.png"), Some(&png(2, 3)));
        assert_eq!(linked.answers(), 1, "and the blocks know to draw again");
        assert!(
            !linked.take("a.png"),
            "a picture in hand is not fetched twice"
        );
    }

    /// A picture that is not there is not there all session: the words say
    /// why, and nothing goes looking for it again.
    #[test]
    fn a_failure_is_the_answer_for_the_rest_of_the_session() {
        let mut linked = Linked::default();
        assert!(linked.take("gone.png"));
        linked.answered(answer("gone.png", Err("not found".into())));
        assert_eq!(linked.failure("gone.png"), Some("not found"));
        assert_eq!(linked.image("gone.png"), None);
        assert!(!linked.take("gone.png"));
    }

    /// An answer for something nobody asked about changes nothing — and, in
    /// particular, does not make the blocks draw again.
    #[test]
    fn an_answer_nobody_asked_for_is_dropped() {
        let mut linked = Linked::default();
        linked.answered(answer("a.png", Ok(png(1, 1))));
        assert_eq!(linked.image("a.png"), None);
        assert_eq!(linked.answers(), 0);
    }

    /// The bound: past `MOST` destinations the words draw their chips and
    /// nothing is read, so the memo can neither grow without end nor send the
    /// run after a hundred files.
    #[test]
    fn a_transcript_of_links_stops_at_the_bound() {
        let mut linked = Linked::default();
        for i in 0..MOST {
            assert!(linked.take(&format!("{i}.png")), "{i}");
        }
        assert!(!linked.take("one-too-many.png"));
        assert_eq!(linked.failure("one-too-many.png"), None, "and says nothing");
    }

    /// Where a word points, and the one rule that keeps this off every other
    /// scheme: only `http(s)` is fetched, and everything else is a path.
    #[test]
    fn a_destination_is_a_path_unless_it_is_a_web_address() {
        let cwd = Path::new("/work");
        let home = Some("/home/me");
        assert_eq!(
            source("docs/x.png", cwd, home),
            Source::Path("/work/docs/x.png".into())
        );
        assert_eq!(
            source("~/shots/x.png", cwd, home),
            Source::Path("/home/me/shots/x.png".into())
        );
        assert_eq!(
            source("https://x.dev/a.png", cwd, home),
            Source::Url("https://x.dev/a.png".into())
        );
        assert!(
            matches!(
                source("data:image/png;base64,AA", cwd, home),
                Source::Path(_)
            ),
            "a scheme this surface does not follow is a path that is not there"
        );
        assert!(matches!(
            source("ftp://x/a.png", cwd, home),
            Source::Path(_)
        ));
    }

    /// The few words a chip can carry, one per kind of failure.
    #[test]
    fn every_failure_has_a_word_short_enough_to_stand_after_a_name() {
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert_eq!(note(&PictureError::Unreadable(missing)), "not found");
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope");
        assert_eq!(note(&PictureError::Unreadable(denied)), "not read");
        assert_eq!(note(&PictureError::NotAPicture), "not a picture");
        assert_eq!(
            note(&PictureError::Refused(bingo_sdk::ImageError::TooLarge {
                bytes: 9,
                max: 8
            })),
            "too large"
        );
        for word in [
            "not found",
            "not read",
            "not fetched",
            "too large",
            "HTTP 404",
        ] {
            assert!(word.len() <= 12, "{word}");
        }
    }

    /// A server's status is the one failure a person can act on from the
    /// note alone — the address is wrong, not the network — so it is spelled.
    #[tokio::test]
    async fn a_servers_answer_is_named_by_its_status() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let answer = read(
            "gone.png".into(),
            Source::Url(format!("{}/gone.png", server.uri())),
            None,
        )
        .await;
        assert_eq!(answer.result.unwrap_err(), "HTTP 404");
    }

    /// A frame's whole list at once, which is what the run hands over: the
    /// ones still to be read come back with where they point, and a second
    /// pass over the same list comes back empty.
    #[test]
    fn a_frames_list_is_taken_once_and_answers_where_each_one_points() {
        let mut linked = Linked::default();
        let wanted = vec!["a.png".to_string(), "https://x.dev/b.png".to_string()];
        let reads = linked.take_all(wanted.clone(), Path::new("/work"), None);
        assert_eq!(
            reads,
            vec![
                ("a.png".to_string(), Source::Path("/work/a.png".into())),
                (
                    "https://x.dev/b.png".to_string(),
                    Source::Url("https://x.dev/b.png".into())
                ),
            ]
        );
        assert!(
            linked.take_all(wanted, Path::new("/work"), None).is_empty(),
            "the next frame names them again and sends nobody"
        );
    }

    /// The picture at a path on this machine, through the seam the run uses:
    /// a file that is there comes back as an `Image`, and one that is not
    /// comes back as the words its chip will wear.
    #[tokio::test]
    async fn a_picture_on_disk_is_read_through_the_seam_and_a_missing_one_says_so() {
        let dir = tempfile::tempdir().expect("a directory");
        let file = dir.path().join("shot.png");
        std::fs::write(&file, bingo_pictures::testing::png_bytes(4, 3)).expect("a picture on disk");

        let read_in = read(
            "shot.png".into(),
            source("shot.png", dir.path(), None),
            None,
        )
        .await;
        assert_eq!(read_in.dest, "shot.png");
        assert_eq!(
            read_in.result.as_ref().map(|i| i.media_type.as_str()),
            Ok("image/png")
        );

        let mut linked = Linked::default();
        assert!(linked.take("shot.png"));
        linked.answered(read_in);
        assert_eq!(
            linked.image("shot.png").map(|i| &i.media_type),
            Some(&"image/png".to_string()),
            "and the memo holds it"
        );

        let missing = read(
            "gone.png".into(),
            source("gone.png", dir.path(), None),
            None,
        )
        .await;
        assert_eq!(missing.result.err(), Some("not found".into()));
    }
}
