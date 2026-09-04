//! The frames the opening is reviewed from.
//!
//! Every beat at both widths: text into insta, so a change to the piece is a
//! diff a person reads, and — behind `--ignored` — the same frames as pictures
//! and the piece itself played in this terminal, so a person can *look* at it.
//! Half of this piece is colour, and colour is the half a snapshot of glyphs
//! cannot show.
//!
//! ```text
//! cargo test -p bingo-surface-tui -- --ignored opening::storyboard::preview
//! cargo test -p bingo-surface-tui --release -- --ignored opening::storyboard::play --nocapture
//! ```

use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::style::Color;
use ratatui::text::Line;

use super::cells::{self, Cell};
use crate::painted::{daylight, in_look, truecolor};
use crate::theme::Theme;

/// The seconds the storyboard is read at: the light running, the light home,
/// the first row arriving, the mark settled, the last row home, and the box.
const AT: [f32; 6] = [0.3, 0.9, 1.1, 1.5, 2.0, 2.4];

/// The two widths every layout rule is written for.
const WIDTHS: [u16; 2] = [80, 120];

fn boxed(width: u16) -> Vec<Line<'static>> {
    crate::welcome::lines(&crate::test_support::state(), usize::from(width), None)
}

/// One frame as a person reads it off the screen. Each row is quoted, as the
/// screen catalogue's are, because half of what this piece does is leave a cell
/// *blank* and a snapshot drops the spaces off the end of a line.
fn drawn(t: f32, width: u16) -> String {
    super::frame(t, &boxed(width))
        .iter()
        .map(|row| format!("\"{row}\""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One frame's name for a file and a snapshot: `shot_1_1_80`.
fn named(t: f32, width: u16) -> String {
    format!("shot_{}_{width}", format!("{t:.1}").replace('.', "_"))
}

#[test]
fn the_twelve_frames_of_the_storyboard() {
    for t in AT {
        for width in WIDTHS {
            insta::assert_snapshot!(named(t, width), in_look(truecolor(), || drawn(t, width)));
        }
    }
}

// ---- the pictures, for a person to look at ------------------------------

/// Where the previews are written. Under the workspace's `target/` because they
/// are build output — looked at once, by whoever asked for them — and spelled
/// from the manifest rather than from the working directory, which a test is
/// run from the crate's own root.
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/opening");

/// How many pixels one cell is drawn as. A terminal cell is about this.
const PIXELS: (u32, u32) = (8, 16);

#[test]
#[ignore = "writes preview files for a person to look at"]
fn preview() {
    let out = std::path::Path::new(OUT);
    std::fs::create_dir_all(out).expect("somewhere to write the previews");
    for t in AT {
        for width in WIDTHS {
            let name = named(t, width);
            std::fs::write(
                out.join(format!("{name}.txt")),
                in_look(truecolor(), || drawn(t, width)),
            )
            .expect("the frame as text");
            for (look, suffix) in [(truecolor(), ""), (daylight(), "_light")] {
                std::fs::write(
                    out.join(format!("{name}{suffix}.png")),
                    in_look_png(look, t, width),
                )
                .expect("the frame");
            }
        }
    }
    println!(
        "opening: {} frames written to {}",
        AT.len() * WIDTHS.len(),
        out.display()
    );
}

/// One frame as a picture, in one look.
fn in_look_png(look: Theme, t: f32, width: u16) -> Vec<u8> {
    crate::theme::with(look, || picture_of(&super::frame(t, &boxed(width))))
}

/// Any drawn frame as a picture: [`PIXELS`] a cell, the ink each cell wears,
/// and the ground the terminal would have been showing behind it.
fn picture_of(rows: &[Line<'static>]) -> Vec<u8> {
    let ground = ground();
    let columns = rows
        .iter()
        .map(|row| cells::of(row).len())
        .max()
        .unwrap_or(0);
    let (width, height) = (columns as u32 * PIXELS.0, rows.len() as u32 * PIXELS.1);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for (y, row) in rows.iter().enumerate() {
        for (x, cell) in cells::of(row).into_iter().enumerate() {
            paint(&mut rgba, width, (x as u32, y as u32), &cell, ground);
        }
    }
    bingo_pictures::testing::png_of(width, height, &rgba)
}

/// What the terminal's own background is under the frame. The design never
/// paints a body background — the terminal's stays — so a picture of a frame
/// has to stand one in for it, and these two are the preview's alone.
fn ground() -> [u8; 3] {
    // Which look is standing, read off the one token that still names a
    // colour either side of it: the tint is a step up from night in the dark
    // look and a step down from paper in the light one. The ink cannot say —
    // it is the terminal's own in both (M73).
    match crate::theme::raised().bg {
        Some(Color::Rgb(r, _, _)) if r < 0x80 => [0x14, 0x11, 0x0e],
        _ => [0xfa, 0xf6, 0xef],
    }
}

/// One cell, as [`PIXELS`] of the picture: its own ground painted over the
/// terminal's, then its ink over whichever part of it the glyph covers.
fn paint(rgba: &mut [u8], width: u32, (x, y): (u32, u32), cell: &Cell, ground: [u8; 3]) {
    let ink = rgb(cell.style.fg).unwrap_or(ground);
    let behind = rgb(cell.style.bg).unwrap_or(ground);
    for down in 0..PIXELS.1 {
        for across in 0..PIXELS.0 {
            let covered = ink_at(&cell.glyph, across, down);
            let pixel = (y * PIXELS.1 + down) * width + (x * PIXELS.0 + across);
            put(rgba, (pixel * 4) as usize, behind, ink, covered);
        }
    }
}

fn rgb(colour: Option<Color>) -> Option<[u8; 3]> {
    match colour {
        Some(Color::Rgb(r, g, b)) => Some([r, g, b]),
        _ => None,
    }
}

fn put(rgba: &mut [u8], start: usize, behind: [u8; 3], ink: [u8; 3], covered: f32) {
    for channel in 0..3 {
        let mixed = f32::from(behind[channel])
            + (f32::from(ink[channel]) - f32::from(behind[channel])) * covered;
        if let Some(slot) = rgba.get_mut(start + channel) {
            *slot = mixed.round().clamp(0.0, 255.0) as u8;
        }
    }
    if let Some(slot) = rgba.get_mut(start + 3) {
        *slot = 0xff;
    }
}

/// How much ink one glyph puts at one pixel of its cell. The box's own strokes
/// are drawn where they actually are, so a border reads as a border and the
/// light running it can be judged as a light and not as a row of numbers.
fn ink_at(glyph: &str, across: u32, down: u32) -> f32 {
    let (left, right) = (across < PIXELS.0 / 2, across >= PIXELS.0 / 2 - 1);
    let (top, bottom) = (down < PIXELS.1 / 2, down >= PIXELS.1 / 2 - 1);
    let along = down >= PIXELS.1 / 2 - 1 && down <= PIXELS.1 / 2;
    let upright = across >= PIXELS.0 / 2 - 1 && across <= PIXELS.0 / 2;
    match glyph {
        " " => 0.0,
        "─" | "-" => f32::from(along),
        "│" | "|" => f32::from(upright),
        "╭" => f32::from((along && right) || (upright && bottom)),
        "╮" => f32::from((along && left) || (upright && bottom)),
        "╰" => f32::from((along && right) || (upright && top)),
        "╯" => f32::from((along && left) || (upright && top)),
        "+" => f32::from(along || upright),
        _ => 0.62,
    }
}

// ---- the piece, played ---------------------------------------------------

/// Play the whole piece in this terminal at the surface's own frame clock, so
/// what is reviewed is the motion and not six stills.
///
/// It writes escapes straight to stdout rather than going through the surface's
/// terminal: there is no session here to run one, and what is being looked at
/// is the brick.
#[test]
#[ignore = "plays the opening in this terminal"]
fn play() {
    let width = crossterm::terminal::size()
        .map(|size| size.0)
        .unwrap_or(100);
    let boxed = boxed(width);
    let mut out = std::io::stdout();
    let _screen = Screen::taken(&mut out);
    let started = Instant::now();
    let mut frames = 0u32;
    let mut drawing = Duration::ZERO;
    while started.elapsed().as_secs_f32() <= super::END {
        let t = started.elapsed().as_secs_f32();
        let began = Instant::now();
        let rows = crate::theme::with(truecolor(), || super::frame(t, &boxed));
        write(&mut out, &rows);
        drawing += began.elapsed();
        frames += 1;
        // Wall-clock, not a frame counter: a late frame is skipped, never
        // played slowly.
        let next = crate::clock::FRAME * frames;
        if let Some(wait) = next.checked_sub(started.elapsed()) {
            std::thread::sleep(wait);
        }
    }
    println!(
        "opening: played {frames} frames in {:?}, {:?} of it drawing ({:?} a frame at {width} columns)",
        started.elapsed(),
        drawing,
        drawing / frames.max(1),
    );
}

/// The alternate screen, given back however the test ends.
struct Screen;

impl Screen {
    fn taken(out: &mut std::io::Stdout) -> Self {
        let _ = out.write_all(b"\x1b[?1049h\x1b[?25l");
        let _ = out.flush();
        Screen
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[?25h\x1b[?1049l\x1b[0m");
        let _ = out.flush();
    }
}

/// One frame, from the home position, a colour change only where a colour
/// changes.
fn write(out: &mut std::io::Stdout, rows: &[Line<'static>]) {
    let mut buffer = String::from("\x1b[H");
    let mut wearing = (None, None);
    for line in rows {
        for span in &line.spans {
            if (span.style.fg, span.style.bg) != wearing {
                wearing = (span.style.fg, span.style.bg);
                buffer.push_str(&paint_of(wearing));
            }
            buffer.push_str(&span.content);
        }
        buffer.push_str("\x1b[K\r\n");
    }
    let _ = out.write_all(buffer.as_bytes());
    let _ = out.flush();
}

fn paint_of((fg, bg): (Option<Color>, Option<Color>)) -> String {
    let mut escape = String::from("\x1b[0m");
    if let Some(Color::Rgb(r, g, b)) = fg {
        escape.push_str(&format!("\x1b[38;2;{r};{g};{b}m"));
    }
    if let Some(Color::Rgb(r, g, b)) = bg {
        escape.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
    }
    escape
}
