//! The ten frames the opening is reviewed from.
//!
//! Two of every shot and both ends of the piece: text into insta, so a change
//! to the shots is a diff a person reads, and — behind `--ignored` — the same
//! ten as pictures, so a person can *look* at them. A storyboard that is only
//! a wall of characters in a snapshot file cannot be judged as a picture, and
//! this milestone is a picture.
//!
//! ```text
//! cargo test -p bingo-surface-tui -- --ignored intro::storyboard::preview
//! cargo test -p bingo-surface-tui --release -- --ignored intro::storyboard::play --nocapture
//! ```

use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::style::Color;

use super::grid::Cell;
use crate::painted::{ascii, daylight, in_look, truecolor};
use crate::theme::Theme;

/// The seconds the storyboard is read at: the first frame of each shot, one
/// inside it, and the last frame of the piece.
const AT: [f32; 10] = [0.0, 0.5, 1.0, 1.6, 2.2, 2.8, 3.2, 3.9, 4.5, 5.0];

/// The size the storyboard is read at — wide enough to be a screen, small
/// enough to be a page.
const BOARD: (u16, u16) = (100, 30);

/// The size the frame budget is held at, which is a large terminal.
const LARGE: (u16, u16) = (120, 40);

const CWD: &str = "/tmp/project";

/// How many times the marching may ask the world where the nearest surface
/// is, for one frame of [`LARGE`].
///
/// Steps and not milliseconds: a step is the same number on a laptop and on
/// CI, and the wall clock is not. The measured time that goes with it is in
/// the plan's Verified section, taken with `--nocapture` on the same test.
///
/// The worst frame of the piece is the corridor, at 181 518 steps — thirty-
/// eight to a cell, most of them the walk down the corridor itself. The
/// budget is that with a third of headroom: a change that needs more than
/// this has changed what the opening costs, and should say so here.
const BUDGET: u64 = 240_000;

fn drawn(t: f32, size: (u16, u16)) -> String {
    super::at(t, size, CWD)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One shot's name for a file and a snapshot: `shot_2_2`.
fn named(t: f32) -> String {
    format!("shot_{}", format!("{t:.1}").replace('.', "_"))
}

#[test]
fn the_ten_frames_of_the_storyboard() {
    for t in AT {
        super::snapshot(&named(t), in_look(truecolor(), || drawn(t, BOARD)));
    }
}

#[test]
fn a_frame_of_a_large_terminal_stays_inside_its_march_budget() {
    let mut worst = (0.0f32, 0u64);
    let mut slowest = Duration::ZERO;
    for step in 0..=50 {
        let t = step as f32 / 10.0;
        let started = Instant::now();
        let steps = crate::theme::with(truecolor(), || super::frame(t, LARGE, CWD).steps);
        let took = started.elapsed();
        slowest = slowest.max(took);
        if steps > worst.1 {
            worst = (t, steps);
        }
    }
    println!(
        "intro: worst frame at 120x40 is t={:.1}s, {} march steps, slowest wall time {:?}",
        worst.0, worst.1, slowest
    );
    assert!(
        worst.1 <= BUDGET,
        "t={:.1}s spent {} march steps (budget {BUDGET})",
        worst.0,
        worst.1
    );
}

#[test]
fn a_shot_is_still_a_shot_on_a_terminal_that_draws_only_ascii() {
    let drawn = in_look(ascii(), || drawn(3.9, BOARD));
    assert!(drawn.is_ascii(), "no glyph outside ASCII:\n{drawn}");
    super::snapshot("shot_3_9_ascii", drawn);
}

// ---- the pictures, for a person to look at ------------------------------

/// Where the previews are written. Under the workspace's `target/` because
/// they are build output — looked at once, by whoever asked for them — and
/// spelled from the manifest rather than from the working directory, which a
/// test is run from the crate's own root.
const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/intro");

/// How many pixels one cell is drawn as. A terminal cell is about this, and
/// the ten frames come out at 800×480 — a size a person can see the whole of.
const PIXELS: (u32, u32) = (8, 16);

#[test]
#[ignore = "writes preview files for a person to look at"]
fn preview() {
    let out = std::path::Path::new(OUT);
    std::fs::create_dir_all(out).expect("somewhere to write the previews");
    for t in AT {
        let name = named(t);
        std::fs::write(
            out.join(format!("{name}.txt")),
            in_look(truecolor(), || drawn(t, BOARD)),
        )
        .expect("the frame as text");
        for (look, suffix) in [(truecolor(), ""), (daylight(), "_light")] {
            let png = in_look_png(look, t);
            std::fs::write(out.join(format!("{name}{suffix}.png")), png).expect("the frame");
        }
    }
    println!("intro: ten frames written to {}", out.display());
}

/// One frame as a picture, in one look.
fn in_look_png(look: Theme, t: f32) -> Vec<u8> {
    let grid = crate::theme::with(look, || super::frame(t, BOARD, CWD).grid);
    crate::theme::with(look, || picture_of(&grid))
}

/// Any canvas as a picture: [`PIXELS`] a cell, the ink each cell wears, and
/// the ground the terminal would have been showing behind it.
pub fn picture_of(grid: &super::grid::Grid) -> Vec<u8> {
    let ground = ground();
    let (width, height) = (
        u32::from(grid.width()) * PIXELS.0,
        u32::from(grid.height()) * PIXELS.1,
    );
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for y in 0..grid.height() {
        for x in 0..grid.width() {
            paint(&mut rgba, width, (x, y), grid.cell(x, y), ground);
        }
    }
    bingo_pictures::testing::png_of(width, height, &rgba)
}

/// What the terminal's own background is under the frame. The design never
/// paints a body background — the terminal's stays — so a picture of a frame
/// has to stand one in for it, and these two are the preview's alone.
fn ground() -> [u8; 3] {
    match crate::theme::text().fg {
        // The warm off-white is the dark look's ink; the near-black is the
        // light look's.
        Some(Color::Rgb(r, _, _)) if r > 0x80 => [0x14, 0x11, 0x0e],
        _ => [0xfa, 0xf6, 0xef],
    }
}

/// One cell, as [`PIXELS`] of the picture.
fn paint(rgba: &mut [u8], width: u32, (x, y): (u16, u16), cell: Cell, ground: [u8; 3]) {
    let ink = match cell.style.fg {
        Some(Color::Rgb(r, g, b)) => [r, g, b],
        _ => ground,
    };
    for down in 0..PIXELS.1 {
        for across in 0..PIXELS.0 {
            let covered = ink_at(cell.glyph, across, down);
            let pixel =
                (u32::from(y) * PIXELS.1 + down) * width + (u32::from(x) * PIXELS.0 + across);
            let start = (pixel * 4) as usize;
            for channel in 0..3 {
                let mixed = f32::from(ground[channel])
                    + (f32::from(ink[channel]) - f32::from(ground[channel])) * covered;
                if let Some(slot) = rgba.get_mut(start + channel) {
                    *slot = mixed.round().clamp(0.0, 255.0) as u8;
                }
            }
            if let Some(slot) = rgba.get_mut(start + 3) {
                *slot = 0xff;
            }
        }
    }
}

/// How much ink one glyph puts at one pixel of its cell.
///
/// A ramp glyph is its own place on the ramp, spread evenly — which is what
/// the eye reads it as from a step back, and reading the frames from a step
/// back is the whole reason these pictures exist. The box's own strokes are
/// drawn where they actually are, so a border reads as a border.
fn ink_at(glyph: char, across: u32, down: u32) -> f32 {
    if let Some(level) = on_ramp(glyph) {
        return level;
    }
    let (left, right) = (across < PIXELS.0 / 2, across >= PIXELS.0 / 2 - 1);
    let (top, bottom) = (down < PIXELS.1 / 2, down >= PIXELS.1 / 2 - 1);
    let along = down >= PIXELS.1 / 2 - 1 && down <= PIXELS.1 / 2;
    let upright = across >= PIXELS.0 / 2 - 1 && across <= PIXELS.0 / 2;
    match glyph {
        ' ' => 0.0,
        '─' | '-' => f32::from(along),
        '│' | '|' => f32::from(upright),
        '╭' => f32::from((along && right) || (upright && bottom)),
        '╮' => f32::from((along && left) || (upright && bottom)),
        '╰' => f32::from((along && right) || (upright && top)),
        '╯' => f32::from((along && left) || (upright && top)),
        '+' => f32::from(along || upright),
        '▌' => f32::from(across < PIXELS.0 / 2),
        _ => 0.62,
    }
}

/// Where a glyph sits on whichever ramp it came from.
fn on_ramp(glyph: char) -> Option<f32> {
    let ramps = [super::shade::RAMP, super::shade::SHADED];
    ramps.iter().find_map(|ramp| {
        ramp.iter()
            .position(|step| *step == glyph)
            .map(|step| step as f32 / (ramp.len() - 1) as f32)
    })
}

// ---- the piece, played ---------------------------------------------------

/// Play the whole five seconds in this terminal at the surface's own frame
/// clock, so what is reviewed is the motion and not ten stills.
///
/// It writes escapes straight to stdout rather than going through the
/// surface's terminal: there is no session here to run one, and what is being
/// looked at is the brick.
#[test]
#[ignore = "plays the opening in this terminal"]
fn play() {
    let size = crossterm::terminal::size().unwrap_or(LARGE);
    let mut out = std::io::stdout();
    let _screen = Screen::taken(&mut out);
    let started = Instant::now();
    let mut frames = 0u32;
    let mut drawing = Duration::ZERO;
    while started.elapsed().as_secs_f32() <= super::scenes::END {
        let t = started.elapsed().as_secs_f32();
        let began = Instant::now();
        let lines = crate::theme::with(truecolor(), || super::at(t, size, CWD));
        write(&mut out, &lines);
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
        "intro: played {frames} frames in {:?}, {:?} of it drawing ({:?} a frame at {}x{})",
        started.elapsed(),
        drawing,
        drawing / frames.max(1),
        size.0,
        size.1
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

/// One frame, from the home position, a colour change only where the colour
/// changes.
fn write(out: &mut std::io::Stdout, lines: &[ratatui::text::Line<'static>]) {
    let mut buffer = String::from("\x1b[H");
    let mut wearing: Option<Color> = None;
    for line in lines {
        for span in &line.spans {
            if span.style.fg != wearing {
                wearing = span.style.fg;
                buffer.push_str(&paint_of(wearing));
            }
            buffer.push_str(&span.content);
        }
        buffer.push_str("\x1b[K\r\n");
    }
    let _ = out.write_all(buffer.as_bytes());
    let _ = out.flush();
}

fn paint_of(colour: Option<Color>) -> String {
    match colour {
        Some(Color::Rgb(r, g, b)) => format!("\x1b[38;2;{r};{g};{b}m"),
        _ => "\x1b[0m".to_string(),
    }
}
