//! Terminal image display: kitty graphics protocol capability detection +
//! sequence building + image loading.
//!
//! One placement scheme only: kitty's Unicode placeholders (`U=1`). The image
//! data is transmitted once per image id; the cells it appears in are ordinary
//! styled text (the placeholder character plus row/column diacritics, the id
//! riding in the foreground colour), painted by the render layer like any
//! other text. Placement therefore survives redraws, scrolling, clipping and
//! multiplexer repaints with no placement bookkeeping at all.
//!
//! Only the transport varies, picked once at detection time:
//! - [`Transport::Bare`] — escape chunks go to the terminal as-is.
//! - [`Transport::Tmux`] — every chunk travels in a tmux passthrough envelope.
//!
//! Terminals that answer the kitty graphics query but lack Unicode-placeholder
//! support (WezTerm, Konsole) are excluded at detection time and keep the
//! `#[image]` text fallback. Images travel as PNG (the protocol only accepts
//! PNG/RGB/RGBA) and are rescaled to the target cell size before transmission.

use std::path::Path;

/// Maximum display width (in cells).
pub const MAX_COLS: u32 = 60;
/// Maximum display height (in cells).
pub const MAX_ROWS: u32 = 18;
/// Size cap for a single image file.
const MAX_BYTES: usize = 10 * 1024 * 1024;
/// Pixel-size cap for images (keeps huge images from blowing up decode and
/// transmission).
const MAX_DIM: u32 = 16_000;

/// Plausible cell pixel bounds. A `14t` answer or a grid size that divides out
/// to something outside these cannot be trusted, so [`ImageCap::default_cells`]
/// is used instead.
const MIN_CELL_W: u32 = 4;
const MAX_CELL_W: u32 = 64;
const MIN_CELL_H: u32 = 6;
const MAX_CELL_H: u32 = 128;

/// kitty graphics capability probe: the `a=q` query action, which terminals
/// implementing the protocol answer with `\e_Gi=31;OK`.
const GRAPHICS_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
/// DA + `14t`: every terminal answers DA, and `14t` carries the text-area pixel
/// size that the cell size is derived from.
const SIZE_QUERY: &[u8] = b"\x1b[c\x1b[14t";
/// How long to wait for the probe answers.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(400);

/// Shown once when the tmux passthrough probe gets no answer. Causes: the
/// outer terminal does not speak the kitty protocol, passthrough could not be
/// enabled, or the pane was not the focused pane during the probe.
const TMUX_PASSTHROUGH_HINT: &str = "under tmux the outer terminal's kitty image support is unconfirmed: the outer terminal must be ghostty/kitty (WezTerm/Konsole do not support placeholders) and bingo must start in the focused pane";

/// Shown once on terminals that answer the kitty graphics query but cannot
/// render Unicode placeholders (WezTerm/Konsole): a probe would pass, the
/// transmit would succeed, and nothing would ever display.
const PLACEHOLDER_UNSUPPORTED_HINT: &str = "this terminal does not support kitty Unicode placeholders (WezTerm/Konsole); images show as #[image]";

/// How transmit chunks reach the terminal that renders the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Escape chunks go out as-is (no multiplexer in between).
    Bare,
    /// Every chunk is wrapped in a tmux passthrough envelope.
    Tmux,
}

/// kitty protocol image capability (including the probed cell size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCap {
    /// Pixel width of one character cell.
    pub cell_w: u32,
    /// Pixel height of one character cell.
    pub cell_h: u32,
    /// Transport the transmit chunks use to reach the rendering terminal.
    pub transport: Transport,
}

/// Outcome of [`detect_image_cap`]: the capability plus an optional one-shot
/// hint for the user (currently only the tmux passthrough hint).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageProbe {
    pub cap: Option<ImageCap>,
    pub warning: Option<String>,
}

impl ImageCap {
    /// Default cell size (fallback when the query fails; Ghostty's default
    /// font is about 8×16).
    pub const fn default_cells() -> Self {
        Self {
            cell_w: 8,
            cell_h: 16,
            transport: Transport::Bare,
        }
    }
}

/// What image detection should do in a given environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbePlan {
    /// No multiplexer: probe the terminal directly. `env_kitty` records the
    /// env-var short circuit for terminals already known to speak the protocol.
    Direct { env_kitty: bool },
    /// Inside tmux: probe through a passthrough envelope. Whether the outer
    /// terminal speaks kitty graphics is decided by the probe answer itself.
    TmuxProbe,
    /// A terminal known to answer the kitty query but lack `U=1` placeholder
    /// support (WezTerm/Konsole), inside tmux or not: a probe would pass and
    /// the image would never display, so keep the `#[image]` text fallback
    /// and say so once.
    Unsupported,
}

/// Detect whether the terminal supports the kitty graphics protocol with
/// Unicode placeholders (and the cell size).
///
/// Fast path: `TERM_PROGRAM=ghostty/kitty` or `TERM=xterm-kitty` decides
/// support directly; WezTerm/Konsole are excluded (no placeholder support);
/// otherwise the terminal is queried (`a=q` query action + DA + 14t pixel
/// size) and support is granted on reading `\x1b_Gi=31;OK`. Must be called
/// before entering raw mode / fullscreen.
///
/// Inside tmux the same `a=q` probe is sent wrapped in a tmux passthrough
/// envelope; an answer means passthrough is on and images can be placed with
/// Unicode placeholders. No answer yields no capability plus a hint (the
/// outer terminal may lack kitty support or passthrough may be off).
pub async fn detect_image_cap() -> ImageProbe {
    let program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    let plan = probe_plan(
        std::env::var_os("TMUX").is_some(),
        program.as_deref(),
        term.as_deref(),
        // tmux overwrites `TERM_PROGRAM` in panes, so the outer terminal is
        // identified only by variables it sets itself; `WEZTERM_EXECUTABLE`
        // and `KONSOLE_VERSION` survive into panes untouched.
        std::env::var_os("WEZTERM_EXECUTABLE").is_some(),
        std::env::var_os("KONSOLE_VERSION").is_some(),
    );
    // The grid size pairs with the `14t` pixel answer to give one cell.
    let grid = crossterm::terminal::size().ok();
    match plan {
        ProbePlan::Unsupported => ImageProbe {
            cap: None,
            warning: Some(PLACEHOLDER_UNSUPPORTED_HINT.to_string()),
        },
        ProbePlan::TmuxProbe => {
            // Best effort: allow the passthrough envelope to reach the outer
            // terminal even when the user has not set `allow-passthrough`
            // themselves (`-p` applies to the current pane only). Without it
            // tmux drops the DCS payloads and nothing displays.
            let _ = std::process::Command::new("tmux")
                .args(["set", "-p", "allow-passthrough", "on"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .and_then(|mut child| child.wait());
            // tmux routes the outer terminal's reply back to the focused
            // pane; bingo probes at startup, i.e. while its pane is focused.
            let wrapped = tmux_passthrough(GRAPHICS_QUERY);
            let buf = query_terminal(&[wrapped.as_slice(), SIZE_QUERY]).await;
            if !buf.as_deref().is_some_and(graphics_query_ok) {
                return ImageProbe {
                    cap: None,
                    warning: Some(TMUX_PASSTHROUGH_HINT.to_string()),
                };
            }
            ImageProbe {
                cap: Some(cap_from(buf.as_deref(), grid, Transport::Tmux)),
                warning: None,
            }
        }
        ProbePlan::Direct { env_kitty } => {
            let buf = query_terminal(&[GRAPHICS_QUERY, SIZE_QUERY]).await;
            if !env_kitty && !buf.as_deref().is_some_and(graphics_query_ok) {
                return ImageProbe::default();
            }
            ImageProbe {
                cap: Some(cap_from(buf.as_deref(), grid, Transport::Bare)),
                warning: None,
            }
        }
    }
}

async fn query_terminal(queries: &[&[u8]]) -> Option<Vec<u8>> {
    crate::tui::theme::Theme::query_terminal(queries, PROBE_TIMEOUT).await
}

/// Env decision matrix.
///
/// WezTerm and Konsole answer the kitty query but lack `U=1` Unicode
/// placeholder placement — the only scheme in use — so a successful probe
/// would transmit the image and never display it. They are excluded first,
/// inside tmux or not. Outside tmux they identify themselves via
/// `TERM_PROGRAM`; inside tmux 3.x overwrites `TERM_PROGRAM` with "tmux", so
/// the exclusion rests on variables the outer terminal sets itself
/// (`WEZTERM_EXECUTABLE` / `KONSOLE_VERSION`, which survive into panes).
///
/// Direct (no tmux): `TERM_PROGRAM`/`TERM` short-circuit terminals already
/// known to render placeholders (ghostty/kitty). Everything else — including
/// every non-excluded outer terminal under tmux — gets a probe whose answer
/// is authoritative.
fn probe_plan(
    in_tmux: bool,
    term_program: Option<&str>,
    term: Option<&str>,
    wezterm: bool,
    konsole: bool,
) -> ProbePlan {
    if wezterm || konsole || matches!(term_program, Some("WezTerm") | Some("konsole")) {
        return ProbePlan::Unsupported;
    }
    if in_tmux {
        ProbePlan::TmuxProbe
    } else {
        ProbePlan::Direct {
            env_kitty: env_kitty(term_program, term),
        }
    }
}

/// Assemble the capability from the probe answers, falling back per B's rules.
fn cap_from(buf: Option<&[u8]>, grid: Option<(u16, u16)>, transport: Transport) -> ImageCap {
    let cells = buf
        .and_then(parse_text_area_px)
        .zip(grid)
        .and_then(|((w, h), (cols, rows))| cells_from_text_area(w, h, cols, rows));
    match cells {
        Some((cell_w, cell_h)) => ImageCap {
            cell_w,
            cell_h,
            transport,
        },
        None => ImageCap {
            transport,
            ..ImageCap::default_cells()
        },
    }
}

/// Decide Unicode-placeholder support from environment variables (pure
/// function, easy to test). Only ghostty and kitty are known to render
/// placeholders; WezTerm/Konsole are excluded earlier in [`probe_plan`].
pub fn env_kitty(term_program: Option<&str>, term: Option<&str>) -> bool {
    match term_program {
        Some("ghostty") | Some("kitty") => true,
        _ => term == Some("xterm-kitty"),
    }
}

/// Whether the query response contains the kitty graphics protocol OK answer
/// (`\x1b_Gi=31;OK`).
fn graphics_query_ok(buf: &[u8]) -> bool {
    buf.windows(b"\x1b_Gi=31;OK".len())
        .any(|w| w == b"\x1b_Gi=31;OK")
}

/// Cell pixel size from the `14t` answer: those pixels span the whole text
/// area, so one cell is that divided by the grid size.
///
/// `None` on a zero grid or an implausible result — the caller then falls back
/// to [`ImageCap::default_cells`]. Reading the raw `14t` numbers as a cell size
/// is what used to squeeze every image into a single cell.
fn cells_from_text_area(
    text_px_w: u32,
    text_px_h: u32,
    cols: u16,
    rows: u16,
) -> Option<(u32, u32)> {
    let (cols, rows) = (u32::from(cols), u32::from(rows));
    if cols == 0 || rows == 0 {
        return None;
    }
    let cell_w = text_px_w / cols;
    let cell_h = text_px_h / rows;
    if !(MIN_CELL_W..=MAX_CELL_W).contains(&cell_w) || !(MIN_CELL_H..=MAX_CELL_H).contains(&cell_h)
    {
        return None;
    }
    Some((cell_w, cell_h))
}

/// Parse a `\x1b[14t` response (`CSI 4 ; height ; width t`) into text-area
/// (width, height) pixels.
fn parse_text_area_px(buf: &[u8]) -> Option<(u32, u32)> {
    let s = std::str::from_utf8(buf).ok()?;
    let start = s.find("\x1b[4;")?;
    let rest = &s[start + 4..];
    let end = rest.find('t')?;
    let mut parts = rest[..end].split(';');
    let h: u32 = parts.next()?.trim().parse().ok()?;
    let w: u32 = parts.next()?.trim().parse().ok()?;
    if h == 0 || w == 0 {
        return None;
    }
    Some((w, h))
}

/// Image pixel size → target cell (cols, rows): scale proportionally to fit
/// the maximum display box, never upscale small images.
pub fn fit_cells(w: u32, h: u32, cap: &ImageCap, max_cols: u32, max_rows: u32) -> (u32, u32) {
    let cw = (w as f64 / cap.cell_w as f64).max(1.0);
    let ch = (h as f64 / cap.cell_h as f64).max(1.0);
    let scale = (max_cols as f64 / cw).min(max_rows as f64 / ch).min(1.0);
    let cols = (cw * scale).round().max(1.0) as u32;
    let rows = (ch * scale).round().max(1.0) as u32;
    (cols, rows)
}

/// Split base64 into kitty `\e_G…\e\\` transmission chunks of 4096 bytes (the
/// protocol cap): the first chunk carries the full control data from
/// `first_header`, continuation chunks only `m`.
fn kitty_chunks(png: &[u8], first_header: &str) -> Vec<Vec<u8>> {
    use base64::Engine;
    const CHUNK: usize = 4096;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let bytes = b64.as_bytes();
    let mut chunks = Vec::with_capacity(bytes.len() / CHUNK + 1);
    let mut start = 0usize;
    loop {
        let end = (start + CHUNK).min(bytes.len());
        let more = end < bytes.len();
        let header = if start == 0 {
            format!("{first_header},m={}", u8::from(more))
        } else {
            format!("m={}", u8::from(more))
        };
        let mut chunk = Vec::with_capacity(header.len() + (end - start) + 6);
        chunk.extend_from_slice(b"\x1b_G");
        chunk.extend_from_slice(header.as_bytes());
        chunk.push(b';');
        chunk.extend_from_slice(&bytes[start..end]);
        chunk.extend_from_slice(b"\x1b\\");
        chunks.push(chunk);
        start = end;
        if !more {
            break;
        }
    }
    chunks
}

/// Wrap a payload in tmux's DCS passthrough. tmux forwards the body to the
/// outer terminal verbatim except that every ESC has to be doubled. Needs
/// tmux >= 3.3 with `allow-passthrough on`, otherwise the body is dropped.
fn tmux_passthrough(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 16);
    out.extend_from_slice(b"\x1bPtmux;");
    for &b in payload {
        if b == 0x1b {
            out.push(0x1b);
        }
        out.push(b);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Transmit an image for virtual placement: `a=T,U=1`, PNG, the placeholder
/// grid size in `c`/`r`, all replies silenced (`q=2` — `q=1` still sends
/// error replies, which reach the event loop as stray input). The transport
/// only decides the envelope: [`Transport::Tmux`] wraps each chunk in its own
/// passthrough envelope (tmux only forwards whole DCS sequences).
///
/// Transmission is position-independent and order-independent: placeholder
/// cells already on screen light up when the data arrives, cells painted
/// later find the data waiting. Nothing here moves the cursor.
pub fn transmit_bytes(
    png: &[u8],
    cols: usize,
    rows: usize,
    id: u32,
    transport: Transport,
) -> Vec<u8> {
    let id = normalize_image_id(id);
    let header = format!("a=T,U=1,q=2,f=100,i={id},c={cols},r={rows}");
    let chunks = kitty_chunks(png, &header);
    match transport {
        Transport::Bare => chunks.concat(),
        Transport::Tmux => chunks
            .iter()
            .flat_map(|chunk| tmux_passthrough(chunk))
            .collect(),
    }
}

/// Unicode placeholder character carrying a virtual placement cell.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics for kitty's Unicode placeholders: index `N` encodes
/// coordinate `N` (0-based, `U+0305` is 0).
///
/// First `max(MAX_COLS, MAX_ROWS)` = 60 codepoints of kitty's authoritative
/// table, in file order, taking the first field of each non-comment line:
/// <https://raw.githubusercontent.com/kovidgoyal/kitty/master/gen/rowcolumn-diacritics.txt>
const ROWCOLUMN_DIACRITICS: [char; 60] = [
    '\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}', '\u{0312}', '\u{033D}', '\u{033E}', '\u{033F}',
    '\u{0346}', '\u{034A}', '\u{034B}', '\u{034C}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035B}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036A}', '\u{036B}', '\u{036C}', '\u{036D}', '\u{036E}', '\u{036F}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059C}', '\u{059D}', '\u{059E}', '\u{059F}', '\u{05A0}', '\u{05A1}',
    '\u{05A8}', '\u{05A9}', '\u{05AB}', '\u{05AC}', '\u{05AF}', '\u{05C4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}',
];

const _: () = assert!(ROWCOLUMN_DIACRITICS.len() >= MAX_COLS as usize);
const _: () = assert!(ROWCOLUMN_DIACRITICS.len() >= MAX_ROWS as usize);

/// The id rides in a 24-bit foreground colour, so only the low three bytes
/// survive; kitty reads 0 as "no id", hence the bump to 1.
fn normalize_image_id(id: u32) -> u32 {
    match id & 0xFF_FFFF {
        0 => 1,
        id => id,
    }
}

/// One placeholder row of `cols` cells: each cell is the placeholder
/// character plus the row's and its column's diacritic (`None` when `row` is
/// beyond the diacritic table; columns are clamped to it). The image id is
/// not part of the text — the render layer carries it in the cells'
/// foreground colour ([`image_id_fg`]).
pub fn placeholder_row_text(row: usize, cols: usize) -> Option<String> {
    let row_mark = *ROWCOLUMN_DIACRITICS.get(row)?;
    let col_marks = &ROWCOLUMN_DIACRITICS[..cols.min(ROWCOLUMN_DIACRITICS.len())];
    let mut out = String::with_capacity(col_marks.len() * 9);
    for &col_mark in col_marks {
        out.push(PLACEHOLDER);
        out.push(row_mark);
        out.push(col_mark);
    }
    Some(out)
}

/// The 24-bit foreground colour that ties placeholder cells to their image id.
pub fn image_id_fg(id: u32) -> (u8, u8, u8) {
    let id = normalize_image_id(id);
    (
        ((id >> 16) & 0xFF) as u8,
        ((id >> 8) & 0xFF) as u8,
        (id & 0xFF) as u8,
    )
}

/// Transmit-once bookkeeping: which image ids the rendering terminal already
/// holds. The terminal's image store outlives frames, so the cache only
/// resets when the store may have been purged (resize / ctrl+l repaint).
#[derive(Debug, Default)]
pub struct Transmits {
    sent: std::collections::HashSet<u32>,
}

impl Transmits {
    /// Whether `id` still needs a transmit (marks it sent).
    pub fn needs(&mut self, id: u32) -> bool {
        self.sent.insert(normalize_image_id(id))
    }

    /// Forget everything (the terminal may have purged its image store).
    pub fn reset(&mut self) {
        self.sent.clear();
    }
}

/// Stable 24-bit image id for an image url. Reusing one id per image means a
/// repaint replaces the terminal-side image instead of piling up a fresh copy
/// per redraw — which a monotonic counter would do, since inline mode reprints
/// whole blocks on every shape change.
pub fn image_id_for(url: &str) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    normalize_image_id((hasher.finish() & 0xFF_FFFF) as u32)
}

/// Load an image from a URL and turn it into a renderer-neutral [`crate::ui::ImageMeta`]:
/// - `data:image/...;base64,` — inline base64
/// - `http(s)://` — download (reqwest)
/// - anything else — local path (relative to cwd)
///
/// Decode → resize (`fit_cells`) → encode PNG. Any failing step returns
/// `None`.
pub async fn load_image(url: &str, cwd: &Path, cap: &ImageCap) -> Option<crate::ui::ImageMeta> {
    let bytes = fetch_bytes(url, cwd).await?;
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    let (cols, rows) = fit_cells(w, h, cap, MAX_COLS, MAX_ROWS);
    let tw = (cols * cap.cell_w).max(1);
    let th = (rows * cap.cell_h).max(1);
    let resized = image::imageops::resize(
        &img.to_rgba8(),
        tw,
        th,
        image::imageops::FilterType::Triangle,
    );
    let mut out = Vec::new();
    resized
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(crate::ui::ImageMeta {
        cols: cols as usize,
        rows: rows as usize,
        bytes: out,
    })
}

/// Fetch the raw bytes by url type.
async fn fetch_bytes(url: &str, cwd: &Path) -> Option<Vec<u8>> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    fetch_bytes_with_home(url, cwd, home.as_deref()).await
}

async fn fetch_bytes_with_home(url: &str, cwd: &Path, home: Option<&Path>) -> Option<Vec<u8>> {
    // CommonMark angle-bracket-wrapped urls (`![alt](<path with spaces>)`)
    // are unwrapped, staying consistent with the render layer's key.
    let url = url
        .strip_prefix('<')
        .and_then(|u| u.strip_suffix('>'))
        .unwrap_or(url);
    if let Some(head) = url.strip_prefix("data:") {
        return decode_data_url(head);
    }
    if let Some(rest) = url.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        let path = percent_encoding::percent_decode_str(rest)
            .decode_utf8()
            .ok()?;
        return std::fs::read(path.as_ref()).ok();
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        // Some CDNs (e.g. Wikimedia) reject clients without a User-Agent with
        // 403; and a non-2xx body is an error page, not image bytes — decoding
        // it would only fail later with a less honest signal.
        let client = reqwest::Client::new();
        let resp = client
            .get(url)
            .header(reqwest::header::USER_AGENT, "bingo")
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
        return resp.bytes().await.ok().map(|b| b.to_vec());
    }
    let path = Path::new(url);
    let path = if let Some(rest) = url.strip_prefix("~/") {
        home?.join(rest)
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    std::fs::read(path).ok()
}

/// Decode `data:[mediatype][;base64],<data>` (base64 variant only).
fn decode_data_url(head: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let comma = head.find(',')?;
    let (meta, data) = head.split_at(comma);
    if !meta.ends_with(";base64") {
        return None;
    }
    let b64 = &data[1..];
    let engine = base64::engine::general_purpose::STANDARD;
    engine
        .decode(b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64))
        .ok()
}

/// Extract image urls from markdown text (`![alt](url)`, url without
/// whitespace).
pub fn extract_image_urls(text: &str) -> Vec<String> {
    // After capture, strip `<>` (CommonMark angle brackets): the render layer
    // (rsmarkdown) strips them too, keeping the same key — otherwise the load
    // cache and the render would disagree.
    let Ok(re) = regex::Regex::new(r"!\[[^\]]*\]\(([^)\s]+)\)") else {
        return Vec::new();
    };
    re.captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .map(|url| {
            url.strip_prefix('<')
                .and_then(|u| u.strip_suffix('>'))
                .map(str::to_string)
                .unwrap_or(url)
        })
        .collect()
}

/// macOS: read PNG bytes from the clipboard when it holds an image (osascript
/// `«class PNGf»`; non-macOS / no image on the clipboard / any failing step
/// returns `None`).
pub fn clipboard_image_png() -> Option<Vec<u8>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let tmp = "/tmp/bingo_clipboard_image.png";
    let _ = std::fs::remove_file(tmp);
    let check = std::process::Command::new("osascript")
        .arg("-e")
        .arg("the clipboard as «class PNGf»")
        .output()
        .ok()?;
    if !check.status.success() {
        return None;
    }
    let script = format!(
        "set png_data to (the clipboard as «class PNGf»)\n\
         set fp to open for access POSIX file \"{tmp}\" with write permission\n\
         write png_data to fp\n\
         close access fp"
    );
    let save = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !save.status.success() {
        return None;
    }
    let bytes = std::fs::read(tmp).ok()?;
    let _ = std::fs::remove_file(tmp);
    (!bytes.is_empty()).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn env_kitty_detects_placeholder_terminals() {
        assert!(env_kitty(Some("ghostty"), None));
        assert!(env_kitty(Some("kitty"), None));
        assert!(env_kitty(None, Some("xterm-kitty")));
        // WezTerm/Konsole speak the protocol but not Unicode placeholders;
        // they are excluded in probe_plan, never short-circuited to support.
        assert!(!env_kitty(Some("WezTerm"), None));
        assert!(!env_kitty(Some("konsole"), None));
        assert!(!env_kitty(Some("iTerm.app"), None));
        assert!(!env_kitty(Some("Apple_Terminal"), Some("xterm-256color")));
        assert!(!env_kitty(None, None));
    }

    #[test]
    fn graphics_query_ok_matches_response() {
        assert!(graphics_query_ok(b"\x1b_Gi=31;OK\x1b\\"));
        assert!(!graphics_query_ok(b"\x1b[?1;2c"));
        assert!(!graphics_query_ok(b""));
    }

    #[test]
    fn parse_text_area_px_parses_14t() {
        // Semantics fix: `CSI 4;height;width t` reports the whole TEXT AREA in
        // pixels, not one cell. The old expectation here (80×40 read as a cell
        // size) encoded the bug that squeezed images into a single cell.
        assert_eq!(parse_text_area_px(b"\x1b[4;982;1512t"), Some((1512, 982)));
        assert_eq!(
            parse_text_area_px(b"junk\x1b[4;25;120tmore"),
            Some((120, 25))
        );
        assert_eq!(parse_text_area_px(b"\x1b[4;0;0t"), None);
        assert_eq!(parse_text_area_px(b"no response"), None);
    }

    #[test]
    fn cells_from_text_area_divides_by_grid() {
        // Ghostty: 1512×982 text area over a 189×60 grid → an 8×16 cell.
        assert_eq!(cells_from_text_area(1512, 982, 189, 60), Some((8, 16)));
        assert_eq!(cells_from_text_area(1600, 900, 100, 50), Some((16, 18)));
    }

    #[test]
    fn cells_from_text_area_rejects_implausible() {
        assert_eq!(
            cells_from_text_area(1512, 982, 0, 60),
            None,
            "divide by zero"
        );
        assert_eq!(
            cells_from_text_area(1512, 982, 189, 0),
            None,
            "divide by zero"
        );
        assert_eq!(
            cells_from_text_area(0, 0, 189, 60),
            None,
            "no pixels reported"
        );
        // The old bug: the text-area size taken straight as the cell size.
        assert_eq!(cells_from_text_area(1512, 982, 1, 1), None);
        assert_eq!(
            cells_from_text_area(300, 982, 100, 60),
            None,
            "cell_w 3 < 4"
        );
        assert_eq!(
            cells_from_text_area(1512, 300, 189, 60),
            None,
            "cell_h 5 < 6"
        );
        assert_eq!(
            cells_from_text_area(6500, 1600, 100, 100),
            None,
            "cell_w 65 > 64"
        );
        assert_eq!(
            cells_from_text_area(800, 12900, 100, 100),
            None,
            "cell_h 129 > 128"
        );
    }

    #[test]
    fn cap_from_pairs_pixels_with_grid() {
        let full = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c\x1b[4;982;1512t";
        assert_eq!(
            cap_from(Some(full), Some((189, 60)), Transport::Bare),
            ImageCap {
                cell_w: 8,
                cell_h: 16,
                transport: Transport::Bare
            }
        );
        // Unknown grid size → defaults, transport preserved.
        assert_eq!(
            cap_from(Some(full), None, Transport::Tmux),
            ImageCap {
                transport: Transport::Tmux,
                ..ImageCap::default_cells()
            }
        );
        // No 14t answer → defaults.
        assert_eq!(
            cap_from(
                Some(b"\x1b_Gi=31;OK\x1b\\"),
                Some((189, 60)),
                Transport::Bare
            ),
            ImageCap::default_cells()
        );
        assert_eq!(
            cap_from(None, Some((189, 60)), Transport::Bare),
            ImageCap::default_cells()
        );
    }

    #[test]
    fn fit_cells_regression_on_text_area_as_cell() {
        // Regression: the 14t numbers used as a cell size collapse everything.
        let bug = ImageCap {
            cell_w: 1512,
            cell_h: 982,
            transport: Transport::Bare,
        };
        assert_eq!(fit_cells(800, 600, &bug, MAX_COLS, MAX_ROWS), (1, 1));
        let fixed = ImageCap {
            cell_w: 8,
            cell_h: 16,
            transport: Transport::Bare,
        };
        assert_eq!(fit_cells(800, 600, &fixed, MAX_COLS, MAX_ROWS), (48, 18));
    }

    #[test]
    fn fit_cells_scales_to_fit_without_upscale() {
        let cap = ImageCap::default_cells();
        // 80×80 pixels = 10×2.5 cells; not upscaled.
        assert_eq!(fit_cells(80, 40, &cap, MAX_COLS, MAX_ROWS), (10, 3));
        // Huge image → shrunk to the max box (within 60×18, proportional).
        assert_eq!(fit_cells(8000, 6000, &cap, MAX_COLS, MAX_ROWS), (48, 18));
        // Small images are not upscaled.
        assert_eq!(fit_cells(16, 16, &cap, MAX_COLS, MAX_ROWS), (2, 1));
        // When row height is the constraint, derive columns from it.
        let (c, r) = fit_cells(4000, 4000, &cap, MAX_COLS, MAX_ROWS);
        assert_eq!(r, 18);
        assert_eq!(c, 36);
    }

    #[test]
    fn transmit_single_chunk_is_virtual_and_cursor_neutral() {
        let out = transmit_bytes(b"abc", 12, 4, 7, Transport::Bare);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,U=1,q=2,f=100,i=7,c=12,r=4,m=0;"));
        assert!(s.ends_with("\x1b\\"));
        assert_eq!(s.matches("\x1b\\").count(), 1);
        assert!(!s.contains('\n'), "transmission does not move the cursor");
        assert!(
            !s.contains("C=1"),
            "the placeholder scheme does no cursor placement"
        );
    }

    #[test]
    fn transmit_chunks_at_4096() {
        // Every 4096 base64 chars = 3072 bytes. 6000 bytes → 2 chunks.
        let png = vec![0u8; 6000];
        let out = transmit_bytes(&png, 10, 2, 7, Transport::Bare);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("\x1b\\").count(), 2);
        assert!(s.contains("m=1;"), "first block has m=1");
        assert!(s.contains("m=0;"), "last block has m=0");
        let first = &s[s.find("m=1;").unwrap() + 4..];
        assert_eq!(
            first.find("\x1b\\").unwrap(),
            4096,
            "first block is 4096 chars"
        );
        // Continuation chunks carry only `m`.
        let second_start = s.find("m=0;").unwrap();
        assert!(
            !s[second_start..].contains("a=T"),
            "continuation blocks carry no control data"
        );
        assert!(s.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn transmit_normalizes_id() {
        let out = transmit_bytes(b"abc", 1, 1, 0, Transport::Bare);
        let s = String::from_utf8(out).unwrap();
        assert!(
            s.contains("i=1,"),
            "id 0 means no id to kitty; normalized to 1: {s}"
        );
        let masked = transmit_bytes(b"abc", 1, 1, 0xFF00_0001, Transport::Bare);
        let s = String::from_utf8(masked).unwrap();
        assert!(
            s.contains("i=1,"),
            "high bytes do not enter the 24-bit id: {s}"
        );
    }

    #[test]
    fn probe_plan_env_matrix() {
        // No tmux: env_kitty short circuit for placeholder-capable terminals.
        assert_eq!(
            probe_plan(false, Some("ghostty"), None, false, false),
            ProbePlan::Direct { env_kitty: true }
        );
        // WezTerm/Konsole are excluded outright — inside tmux or not — via
        // TERM_PROGRAM (reliable outside tmux) or their own env vars: they
        // answer the query but never render Unicode placeholders.
        assert_eq!(
            probe_plan(false, Some("WezTerm"), None, false, false),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(false, Some("konsole"), None, false, false),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(false, None, None, true, false),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(
                false,
                Some("Apple_Terminal"),
                Some("xterm-256color"),
                false,
                false
            ),
            ProbePlan::Direct { env_kitty: false }
        );
        // Inside tmux, `TERM_PROGRAM` is overwritten to "tmux" by tmux 3.x and
        // `TERM` is the pane's own, so they no longer decide anything: every
        // outer terminal we cannot positively exclude gets a passthrough probe
        // and the query answer is authoritative.
        assert_eq!(
            probe_plan(true, Some("ghostty"), None, false, false),
            ProbePlan::TmuxProbe
        );
        assert_eq!(
            probe_plan(true, Some("kitty"), None, false, false),
            ProbePlan::TmuxProbe
        );
        assert_eq!(
            probe_plan(true, None, Some("xterm-kitty"), false, false),
            ProbePlan::TmuxProbe
        );
        assert_eq!(
            probe_plan(true, Some("tmux"), Some("tmux-256color"), false, false),
            ProbePlan::TmuxProbe
        );
        assert_eq!(
            probe_plan(true, None, None, false, false),
            ProbePlan::TmuxProbe
        );
        assert_eq!(
            probe_plan(
                true,
                Some("Apple_Terminal"),
                Some("screen-256color"),
                false,
                false
            ),
            ProbePlan::TmuxProbe,
            "screen does not answer the query, so the probe fails on its own"
        );
        // Inside tmux the exclusion rests on env vars the outer terminal sets
        // itself (tmux overwrites TERM_PROGRAM in panes).
        assert_eq!(
            probe_plan(true, Some("WezTerm"), None, true, false),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(true, Some("konsole"), None, false, true),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(true, None, None, true, false),
            ProbePlan::Unsupported
        );
        assert_eq!(
            probe_plan(true, None, None, false, true),
            ProbePlan::Unsupported
        );
    }

    #[test]
    fn graphics_query_ok_reads_passthrough_reply() {
        // The outer terminal's answer arrives unwrapped on the pane's stdin.
        assert!(graphics_query_ok(
            b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c\x1b[4;982;1512t"
        ));
        // passthrough off: DA/14t still answer, the graphics query does not.
        assert!(!graphics_query_ok(b"\x1b[?62;c\x1b[4;982;1512t"));
    }

    #[test]
    fn tmux_passthrough_doubles_escapes() {
        assert_eq!(
            tmux_passthrough(b"\x1b_Gq\x1b\\"),
            b"\x1bPtmux;\x1b\x1b_Gq\x1b\x1b\\\x1b\\".to_vec()
        );
        assert_eq!(tmux_passthrough(b""), b"\x1bPtmux;\x1b\\".to_vec());
        assert_eq!(
            tmux_passthrough(b"plain"),
            b"\x1bPtmux;plain\x1b\\".to_vec()
        );
    }

    #[test]
    fn tmux_transport_wraps_every_chunk() {
        // 6000 bytes → 8000 base64 chars → 2 chunks, 2 envelopes.
        let png = vec![0u8; 6000];
        let out = transmit_bytes(&png, 10, 2, 0x01_0203, Transport::Tmux);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("\x1bPtmux;").count(), 2, "one envelope per chunk");
        assert_eq!(s.matches("\x1b_G").count(), 2);
        assert_eq!(
            s.matches("\x1b\x1b_G").count(),
            2,
            "every ESC in the body doubled"
        );
        assert!(s.contains("\x1b\x1b_Ga=T,U=1,q=2,f=100,i=66051,c=10,r=2,m=1;"));
        assert!(
            s.contains("\x1b\x1b_Gm=0;"),
            "continuation chunk carries only m"
        );
        assert!(s.ends_with("\x1b\\"));
        assert!(!s.contains('\n'), "transport alone never moves the cursor");
        // The transports differ only in the envelope: unwrapping each
        // envelope (strip the DCS frame, undouble ESC) yields the bare bytes.
        let bare = String::from_utf8(transmit_bytes(&png, 10, 2, 0x01_0203, Transport::Bare))
            .unwrap_or_default();
        let unwrapped: String = s
            .split("\x1bPtmux;")
            .filter(|part| !part.is_empty())
            .map(|part| {
                part.strip_suffix("\x1b\\")
                    .unwrap_or(part)
                    .replace("\x1b\x1b", "\x1b")
            })
            .collect();
        assert_eq!(unwrapped, bare);
    }

    #[test]
    fn placeholder_row_text_encodes_coordinates() {
        let row0 = placeholder_row_text(0, 3).expect("row 0 within table");
        assert_eq!(
            row0,
            format!(
                "{PLACEHOLDER}\u{305}\u{305}{PLACEHOLDER}\u{305}\u{30d}{PLACEHOLDER}\u{305}\u{30e}"
            ),
            "row diacritic first, then the column's, 0-based into kitty's table"
        );
        let row1 = placeholder_row_text(1, 2).expect("row 1 within table");
        assert_eq!(
            row1,
            format!("{PLACEHOLDER}\u{30d}\u{305}{PLACEHOLDER}\u{30d}\u{30d}")
        );
        // Beyond the table: rows fail, columns clamp — never panic.
        assert_eq!(placeholder_row_text(ROWCOLUMN_DIACRITICS.len(), 3), None);
        let clamped = placeholder_row_text(0, 1000).expect("cols clamp");
        assert_eq!(
            clamped.chars().filter(|c| *c == PLACEHOLDER).count(),
            ROWCOLUMN_DIACRITICS.len()
        );
    }

    #[test]
    fn image_id_fg_normalizes() {
        assert_eq!(image_id_fg(0x0A_0B0C), (10, 11, 12));
        // id 0 means "no id" to kitty; the high byte never survives the fg.
        assert_eq!(image_id_fg(0), (0, 0, 1));
        assert_eq!(image_id_fg(0xFF00_0000), (0, 0, 1));
        assert_eq!(image_id_fg(0x01FF_FFFF), (255, 255, 255));
    }

    #[test]
    fn transmits_cache_sends_once_until_reset() {
        let mut transmits = Transmits::default();
        assert!(transmits.needs(7), "first sight needs a transmit");
        assert!(!transmits.needs(7), "already in the terminal's store");
        assert!(transmits.needs(8), "distinct id transmits on its own");
        // ids 0 and 1 are the same image after normalization.
        assert!(transmits.needs(0));
        assert!(!transmits.needs(1));
        transmits.reset();
        assert!(transmits.needs(7), "reset forgets the purged store");
    }

    #[test]
    fn diacritics_table_matches_kitty() {
        assert_eq!(ROWCOLUMN_DIACRITICS.len(), MAX_COLS as usize);
        assert_eq!(
            ROWCOLUMN_DIACRITICS[0], '\u{305}',
            "index 0 encodes coordinate 0"
        );
        assert_eq!(ROWCOLUMN_DIACRITICS[1], '\u{30d}');
        assert_eq!(ROWCOLUMN_DIACRITICS[59], '\u{615}');
        assert!(
            ROWCOLUMN_DIACRITICS.windows(2).all(|w| w[0] < w[1]),
            "kitty's table is codepoint-ordered and duplicate-free"
        );
    }

    #[test]
    fn data_url_decode() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG");
        let head = format!("image/png;base64,{b64}");
        assert_eq!(decode_data_url(&head), Some(b"\x89PNG".to_vec()));
        assert_eq!(decode_data_url("image/png,notbase64"), None);
        assert_eq!(decode_data_url("image/png;base64,!@#"), None);
    }

    #[test]
    fn extract_image_urls_finds_markdown_images() {
        assert_eq!(
            extract_image_urls("see ![img](a.png) and ![b](https://x.com/i.png) here"),
            vec!["a.png".to_string(), "https://x.com/i.png".to_string()]
        );
        // Angle-bracket-wrapped urls (CommonMark `<...>`) are stripped,
        // consistent with the render layer's key.
        assert_eq!(
            extract_image_urls("![img](</Users/x/Untitled-1.png>)"),
            vec!["/Users/x/Untitled-1.png".to_string()]
        );
        assert_eq!(extract_image_urls("no images"), Vec::<String>::new());
        assert_eq!(
            extract_image_urls("![alt](has space.png)"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn load_image_from_data_url() {
        let cap = ImageCap::default_cells();
        let png = tiny_png();
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        let meta = tokio::runtime::Runtime::new().unwrap().block_on(load_image(
            &url,
            Path::new("."),
            &cap,
        ));
        let meta = meta.expect("data url png loads");
        assert!(meta.cols >= 1 && meta.rows >= 1);
        assert!(meta.bytes.starts_with(b"\x89PNG"));
    }

    #[test]
    fn fetch_bytes_decodes_file_urls() {
        let tmp = std::env::temp_dir().join(format!("bingo-gfx-fileurl-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = tmp.join("Weixin Image.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        // file:// + percent-encoding (models often write paths with spaces as
        // %20).
        let url = format!("file://{}", path.display());
        let encoded = url.replace(' ', "%20");
        let bytes = runtime.block_on(fetch_bytes(&encoded, Path::new("/nonexistent")));
        assert_eq!(
            bytes,
            Some(b"\x89PNG\r\n\x1a\n".to_vec()),
            "file url decodes"
        );
        // Relative file paths resolve against cwd.
        let rel = runtime.block_on(fetch_bytes("sub/img.png", &tmp));
        assert_eq!(rel, None, "missing relative paths fail");
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("sub/img.png"), b"x").unwrap();
        let rel = runtime.block_on(fetch_bytes("sub/img.png", &tmp));
        assert_eq!(
            rel,
            Some(b"x".to_vec()),
            "relative paths resolve against cwd"
        );
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join("Documents")).unwrap();
        std::fs::write(home.join("Documents/image.png"), b"home").unwrap();
        let tilde = runtime.block_on(fetch_bytes_with_home(
            "~/Documents/image.png",
            Path::new("/nonexistent"),
            Some(&home),
        ));
        assert_eq!(
            tilde,
            Some(b"home".to_vec()),
            "tilde paths resolve against HOME"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_image_rejects_garbage() {
        let cap = ImageCap::default_cells();
        let url = "data:image/png;base64,AAAA".to_string();
        let meta = tokio::runtime::Runtime::new().unwrap().block_on(load_image(
            &url,
            Path::new("."),
            &cap,
        ));
        assert!(meta.is_none());
    }

    /// HTTP fetches carry a User-Agent (some CDNs, e.g. Wikimedia, 403
    /// anonymous clients) and non-2xx responses yield None instead of handing
    /// an error page to the image decoder.
    #[tokio::test]
    async fn http_fetch_sends_user_agent_and_rejects_error_status() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut agents = Vec::new();
            for (i, stream) in listener.incoming().take(2).enumerate() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut agent = None;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("user-agent:") {
                        agent = Some(value.trim().to_string());
                    }
                }
                agents.push(agent);
                let resp = if i == 0 {
                    "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nimg"
                } else {
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                stream.write_all(resp.as_bytes()).unwrap();
            }
            agents
        });

        let url = format!("http://{addr}/a.png");
        let ok = fetch_bytes_with_home(&url, Path::new("."), None).await;
        assert_eq!(
            ok.as_deref(),
            Some(b"img".as_slice()),
            "2xx returns the response body bytes"
        );
        let denied = fetch_bytes_with_home(&url, Path::new("."), None).await;
        assert!(denied.is_none(), "non-2xx must not reach image decoding");
        let agents = server.join().unwrap();
        assert!(
            agents.iter().all(|a| a.as_deref() == Some("bingo")),
            "both requests carry User-Agent: {agents:?}"
        );
    }

    /// A 4×2 solid-colour PNG (for tests).
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }
}
