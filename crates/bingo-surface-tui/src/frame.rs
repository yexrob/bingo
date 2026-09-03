//! Where each region of the screen is, and nothing about what goes in them.
//!
//! Nothing sits above the transcript: it starts at row 0 and grows down to a
//! fixed baseline — the activity rows, the input box and the one status line —
//! which are cut from the bottom first, so the box never moves when the
//! transcript does. Past [`RAIL_AT`] columns the transcript gives its right
//! edge to the rail.
//!
//! It is pure geometry over a [`Demand`], so every size is a table row in a
//! test rather than a screen someone has to look at.

use ratatui::layout::Rect;

/// Columns from which the rail is drawn beside the transcript.
pub const RAIL_AT: u16 = 120;
/// How wide the rail is, its left gutter included.
pub const RAIL_WIDTH: u16 = 24;
/// The input box at its smallest: two borders and one row to type in.
pub const COMPOSER_MIN: u16 = 3;
/// The input box at its largest: two borders and ten rows.
pub const COMPOSER_MAX: u16 = 12;

/// What the frame must make room for, measured before the geometry is cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Demand {
    /// Rows of text in the input box, before its border.
    pub composer: u16,
    /// Rows of thumbnails inside the box above the prompt, when the draft
    /// carries pictures a terminal can draw (M48).
    pub strip: u16,
    /// Rows between the transcript and the box: the activity row and whatever
    /// is queued behind it.
    pub activity: u16,
    /// Something has a card for the rail.
    pub rail: bool,
}

/// The frame, top to bottom. Every region may be empty; the status line and
/// the input box are the last to go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Regions {
    pub transcript: Rect,
    /// The rail, past [`RAIL_AT`] columns and only when something wants it.
    pub rail: Option<Rect>,
    pub activity: Rect,
    pub composer: Rect,
    pub status: Rect,
}

impl Regions {
    /// Everything above the input box: what a layer may cover.
    pub fn above(&self) -> Rect {
        let band = self.transcript.union(self.activity);
        // `union` of an empty rect with another is not the other, so an empty
        // transcript would drag the band back to row 0 with no height.
        match (self.transcript.height, self.activity.height) {
            (0, _) => self.activity,
            (_, 0) => self.transcript,
            _ => band,
        }
    }
}

/// Cut the frame into its regions from the bottom up.
pub fn regions(size: Rect, demand: Demand) -> Regions {
    let mut rest = size;
    let status = take_bottom(&mut rest, 1);
    let rows = composer_rows(demand.composer, demand.strip, rest.height);
    let composer = take_bottom(&mut rest, rows);
    let rows = activity_rows(demand.activity, rest.height);
    let activity = take_bottom(&mut rest, rows);
    let (transcript, rail) = split_rail(rest, demand.rail);
    Regions {
        transcript,
        rail,
        // The box and the rows above it are the width of the transcript: the
        // rail's column is the rail's for the whole of its height.
        activity: narrow(activity, transcript.width),
        composer: narrow(composer, transcript.width),
        status,
    }
}

/// The box grows with the draft and stops at ten rows; it never shrinks below
/// one row, and it takes what is left when even that does not fit.
///
/// The strip is rows on top of those ten rather than out of them: a picture
/// pasted into a full draft must not push a line of it off the screen.
fn composer_rows(text: u16, strip: u16, room: u16) -> u16 {
    text.saturating_add(2)
        .clamp(COMPOSER_MIN, COMPOSER_MAX)
        .saturating_add(strip)
        .min(room)
}

/// The activity rows yield before the transcript's last row does.
fn activity_rows(want: u16, room: u16) -> u16 {
    want.min(room.saturating_sub(1))
}

fn split_rail(band: Rect, wanted: bool) -> (Rect, Option<Rect>) {
    if !wanted || band.width < RAIL_AT {
        return (band, None);
    }
    let rail = Rect {
        x: band.right() - RAIL_WIDTH,
        width: RAIL_WIDTH,
        ..band
    };
    let transcript = Rect {
        width: band.width - RAIL_WIDTH,
        ..band
    };
    (transcript, Some(rail))
}

fn narrow(area: Rect, width: u16) -> Rect {
    Rect {
        width: width.min(area.width),
        ..area
    }
}

/// Take `height` rows off the bottom of `rest`, or as many as there are.
fn take_bottom(rest: &mut Rect, height: u16) -> Rect {
    let height = height.min(rest.height);
    rest.height -= height;
    Rect {
        y: rest.bottom(),
        height,
        ..*rest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demand() -> Demand {
        Demand {
            composer: 1,
            strip: 0,
            activity: 0,
            rail: true,
        }
    }

    fn at(width: u16, height: u16, demand: Demand) -> Regions {
        regions(Rect::new(0, 0, width, height), demand)
    }

    #[test]
    fn the_four_sizes_lay_out_the_same_frame() {
        // width, height, transcript, rail, composer, status
        let table = [
            (80u16, 24u16, (80u16, 20u16), None, (80u16, 3u16), 23u16),
            (100, 30, (100, 26), None, (100, 3), 29),
            (120, 40, (96, 36), Some((96u16, 24u16)), (96, 3), 39),
            (200, 60, (176, 56), Some((176, 24)), (176, 3), 59),
        ];
        for (width, height, transcript, rail, composer, status) in table {
            let r = at(width, height, demand());
            assert_eq!(
                (r.transcript.width, r.transcript.height),
                transcript,
                "{width}x{height} transcript"
            );
            assert_eq!(
                r.rail.map(|rail| (rail.x, rail.width)),
                rail,
                "{width}x{height} rail"
            );
            assert_eq!(r.transcript.y, 0, "nothing sits above the transcript");
            assert_eq!(
                (r.composer.width, r.composer.height),
                composer,
                "{width}x{height} composer"
            );
            assert_eq!(r.status.y, status, "{width}x{height} status");
            assert_eq!(r.status.height, 1);
            assert_eq!(r.status.width, width, "the status line spans the frame");
        }
    }

    #[test]
    fn the_rail_appears_at_a_hundred_and_twenty_columns_and_not_before() {
        assert!(at(RAIL_AT - 1, 40, demand()).rail.is_none());
        assert!(at(RAIL_AT, 40, demand()).rail.is_some());
        assert!(
            at(
                RAIL_AT,
                40,
                Demand {
                    rail: false,
                    ..demand()
                }
            )
            .rail
            .is_none(),
            "an empty rail is not drawn"
        );
    }

    #[test]
    fn the_composer_and_the_status_line_survive_the_smallest_screens() {
        for (width, height) in [(20u16, 5u16), (10, 3), (4, 2), (1, 1)] {
            let r = at(width, height, demand());
            assert_eq!(r.status.height, 1, "{width}x{height}");
            assert_eq!(
                r.composer.height,
                (height - 1).min(COMPOSER_MIN),
                "{width}x{height}"
            );
            let used =
                r.transcript.height + r.activity.height + r.composer.height + r.status.height;
            assert_eq!(used, height, "{width}x{height} leaves no gap");
        }
    }

    #[test]
    fn the_box_grows_with_the_draft_and_stops_at_ten_rows() {
        let rows = |text| {
            at(
                80,
                40,
                Demand {
                    composer: text,
                    ..demand()
                },
            )
            .composer
            .height
        };
        assert_eq!(rows(0), COMPOSER_MIN);
        assert_eq!(rows(1), COMPOSER_MIN);
        assert_eq!(rows(4), 6);
        assert_eq!(rows(10), COMPOSER_MAX);
        assert_eq!(rows(40), COMPOSER_MAX, "it never eats the transcript");
    }

    /// A strip of thumbnails is rows of its own on top of the ten: a picture
    /// pasted into a full draft costs the transcript, never the draft.
    #[test]
    fn a_strip_grows_the_box_without_eating_the_ten_rows_of_draft() {
        let rows = |text, strip| {
            at(
                80,
                40,
                Demand {
                    composer: text,
                    strip,
                    ..demand()
                },
            )
            .composer
            .height
        };
        assert_eq!(rows(1, 3), COMPOSER_MIN + 3);
        assert_eq!(rows(10, 3), COMPOSER_MAX + 3);
        assert_eq!(rows(40, 3), COMPOSER_MAX + 3);
        assert_eq!(
            rows(1, 0),
            COMPOSER_MIN,
            "and none of it when there is none"
        );
    }

    #[test]
    fn the_activity_rows_give_way_before_the_transcript_does() {
        let r = at(
            80,
            24,
            Demand {
                activity: 3,
                ..demand()
            },
        );
        assert_eq!(r.activity.height, 3);
        assert_eq!(r.transcript.height, 17);

        let tight = at(
            80,
            6,
            Demand {
                activity: 3,
                ..demand()
            },
        );
        assert_eq!(tight.activity.height, 1, "one row was all there was");
        assert_eq!(tight.transcript.height, 1);
    }

    #[test]
    fn every_region_stacks_without_overlapping() {
        let r = at(
            120,
            40,
            Demand {
                composer: 2,
                strip: 0,
                activity: 2,
                rail: true,
            },
        );
        assert_eq!(r.transcript.bottom(), r.activity.y);
        assert_eq!(r.activity.bottom(), r.composer.y);
        assert_eq!(r.composer.bottom(), r.status.y);
        let rail = r.rail.expect("a rail at 120 columns");
        assert_eq!(rail.y, r.transcript.y);
        assert_eq!(rail.height, r.transcript.height);
        assert_eq!(r.transcript.right(), rail.x, "no gap between them");
        assert_eq!(
            r.above(),
            Rect::new(0, 0, 96, r.transcript.height + r.activity.height)
        );
    }
}
