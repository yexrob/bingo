//! 终端图片显示：kitty graphics protocol 能力检测 + 序列构建 + 图片加载。
//!
//! 只做 kitty 协议（Ghostty/kitty/WezTerm/Konsole 均支持）；其余终端
//! 由渲染层显示 `#[image]` 占位。图片以 PNG 传输（协议只认 PNG/RGB/RGBA），
//! 传输前按 cell 尺寸缩放到目标单元格尺寸。
//!
//! Placement comes in two flavours, picked once at detection time:
//! - [`ImageMode::Direct`] — bare kitty escapes with `C=1`, used when nothing
//!   sits between us and the terminal.
//! - [`ImageMode::TmuxPlaceholder`] — inside tmux every escape chunk travels in
//!   a tmux passthrough envelope and the image is placed with kitty's Unicode
//!   placeholders (`U=1`), the scheme kitty designed for multiplexers.

use std::path::Path;

/// 最大显示宽度（单元格列数）。
pub const MAX_COLS: u32 = 60;
/// 最大显示高度（单元格行数）。
pub const MAX_ROWS: u32 = 18;
/// 单个图片文件大小上限。
const MAX_BYTES: usize = 10 * 1024 * 1024;
/// 图片像素尺寸上限（防止超大图撑爆解码与传输）。
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

/// Shown once when tmux fronts a placeholder-capable terminal but swallows the
/// passthrough probe — almost always `allow-passthrough` being off.
const TMUX_PASSTHROUGH_HINT: &str =
    "tmux 检测到 kitty 终端但 passthrough 未开启：`tmux set -g allow-passthrough on` 可启用图片";

/// How an image is placed on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMode {
    /// Bare kitty escapes, `C=1` placement (no multiplexer in between).
    Direct,
    /// tmux passthrough transport + kitty Unicode placeholders (`U=1`).
    TmuxPlaceholder,
}

/// kitty 协议图片能力（含探测到的 cell 尺寸）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCap {
    /// 单个字符单元格的像素宽。
    pub cell_w: u32,
    /// 单个字符单元格的像素高。
    pub cell_h: u32,
    /// Transport + placement scheme to use for this terminal.
    pub mode: ImageMode,
}

/// Outcome of [`detect_image_cap`]: the capability plus an optional one-shot
/// hint for the user (currently only the tmux passthrough hint).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageProbe {
    pub cap: Option<ImageCap>,
    pub warning: Option<String>,
}

/// 一张已加载图片：目标单元格尺寸 + PNG 字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMeta {
    pub cols: usize,
    pub rows: usize,
    pub bytes: Vec<u8>,
}

impl ImageCap {
    /// 默认 cell 尺寸（查询失败时回落；Ghostty 默认字体约 8×16）。
    pub const fn default_cells() -> Self {
        Self { cell_w: 8, cell_h: 16, mode: ImageMode::Direct }
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
    /// Inside tmux, fronted by a terminal known to lack `U=1` placeholder
    /// support (WezTerm/Konsole/screen): images cannot survive the
    /// multiplexer, so keep the `#[image]` placeholder.
    TmuxUnsupported,
}

/// 检测终端是否支持 kitty graphics protocol（及 cell 尺寸）。
///
/// 快速路径：`TERM_PROGRAM=ghostty/WezTerm/kitty/konsole` 或
/// `TERM=xterm-kitty` 直接判定支持；否则向终端发起协议查询
/// （`a=q` 查询动作 + DA + 14t 像素尺寸），读到 `\x1b_Gi=31;OK`
/// 即支持。需在进 raw mode / 全屏前调用。
///
/// Inside tmux the same `a=q` probe is sent wrapped in a tmux passthrough
/// envelope; an answer means passthrough is on and images can be placed with
/// Unicode placeholders. No answer yields no capability plus a hint, because
/// the usual cause is `allow-passthrough` being off.
pub async fn detect_image_cap() -> ImageProbe {
    let program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    let plan = probe_plan(
        std::env::var_os("TMUX").is_some(),
        program.as_deref(),
        term.as_deref(),
    );
    // The grid size pairs with the `14t` pixel answer to give one cell.
    let grid = crossterm::terminal::size().ok();
    match plan {
        ProbePlan::TmuxUnsupported => ImageProbe::default(),
        ProbePlan::TmuxProbe => {
            let wrapped = tmux_passthrough(GRAPHICS_QUERY);
            let buf = query_terminal(&[wrapped.as_slice(), SIZE_QUERY]).await;
            if !buf.as_deref().is_some_and(graphics_query_ok) {
                return ImageProbe {
                    cap: None,
                    warning: Some(TMUX_PASSTHROUGH_HINT.to_string()),
                };
            }
            ImageProbe {
                cap: Some(cap_from(buf.as_deref(), grid, ImageMode::TmuxPlaceholder)),
                warning: None,
            }
        }
        ProbePlan::Direct { env_kitty } => {
            let buf = query_terminal(&[GRAPHICS_QUERY, SIZE_QUERY]).await;
            if !env_kitty && !buf.as_deref().is_some_and(graphics_query_ok) {
                return ImageProbe::default();
            }
            ImageProbe {
                cap: Some(cap_from(buf.as_deref(), grid, ImageMode::Direct)),
                warning: None,
            }
        }
    }
}

async fn query_terminal(queries: &[&[u8]]) -> Option<Vec<u8>> {
    crate::tui::theme::Theme::query_terminal(queries, PROBE_TIMEOUT).await
}

/// Env decision matrix (`TERM_PROGRAM` survives tmux, `TERM` does not).
fn probe_plan(in_tmux: bool, term_program: Option<&str>, term: Option<&str>) -> ProbePlan {
    if !in_tmux {
        return ProbePlan::Direct { env_kitty: env_kitty(term_program, term) };
    }
    if outer_supports_placeholders(term_program, term) {
        ProbePlan::TmuxProbe
    } else {
        ProbePlan::TmuxUnsupported
    }
}

/// Terminals implementing kitty's Unicode placeholder placement (`U=1`), the
/// only placement that works behind a multiplexer. WezTerm and Konsole speak
/// the graphics protocol but not placeholders, so they get no probe at all.
fn outer_supports_placeholders(term_program: Option<&str>, term: Option<&str>) -> bool {
    matches!(term_program, Some("ghostty") | Some("kitty")) || term == Some("xterm-kitty")
}

/// Assemble the capability from the probe answers, falling back per B's rules.
fn cap_from(buf: Option<&[u8]>, grid: Option<(u16, u16)>, mode: ImageMode) -> ImageCap {
    let cells = buf
        .and_then(parse_text_area_px)
        .zip(grid)
        .and_then(|((w, h), (cols, rows))| cells_from_text_area(w, h, cols, rows));
    match cells {
        Some((cell_w, cell_h)) => ImageCap { cell_w, cell_h, mode },
        None => ImageCap { mode, ..ImageCap::default_cells() },
    }
}

/// 从环境变量判断 kitty 协议支持（纯函数，便于测试）。
pub fn env_kitty(term_program: Option<&str>, term: Option<&str>) -> bool {
    match term_program {
        Some("ghostty") | Some("WezTerm") | Some("kitty") | Some("konsole") => true,
        _ => term == Some("xterm-kitty"),
    }
}

/// 查询响应中是否含 kitty 图形协议 OK 应答（`\x1b_Gi=31;OK`）。
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
    if !(MIN_CELL_W..=MAX_CELL_W).contains(&cell_w)
        || !(MIN_CELL_H..=MAX_CELL_H).contains(&cell_h)
    {
        return None;
    }
    Some((cell_w, cell_h))
}

/// 解析 `\x1b[14t` 响应（`CSI 4 ; height ; width t`）为文本区 (宽, 高) 像素。
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

/// 图片像素尺寸 → 目标单元格 (cols, rows)：等比缩放适配最大显示框，
/// 不放大小图。
pub fn fit_cells(
    w: u32,
    h: u32,
    cap: &ImageCap,
    max_cols: u32,
    max_rows: u32,
) -> (u32, u32) {
    let cw = (w as f64 / cap.cell_w as f64).max(1.0);
    let ch = (h as f64 / cap.cell_h as f64).max(1.0);
    let scale = (max_cols as f64 / cw).min(max_rows as f64 / ch).min(1.0);
    let cols = (cw * scale).round().max(1.0) as u32;
    let rows = (ch * scale).round().max(1.0) as u32;
    (cols, rows)
}

/// base64 按 4096 字节分块（协议上限）切成 kitty `\e_G…\e\\` 传输块：
/// 首块携带 `first_header` 的完整控制数据，续块只带 `m`。
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

/// 构建 kitty 传输+放置序列：首块控制数据为 `a=T` 传输并显示、PNG、
/// 静默 OK 应答、不移动光标。末尾追加 `rows` 个 `\r\n` 推进光标
/// （`C=1` 放置不移动光标；raw mode 下裸 `\n` 只下移不回车，必须带 CR）。
pub fn kitty_image_bytes(png: &[u8], cols: usize, rows: usize) -> Vec<u8> {
    let header = format!("a=T,f=100,q=1,c={cols},r={rows},C=1");
    let mut out: Vec<u8> = kitty_chunks(png, &header).concat();
    for _ in 0..rows {
        out.extend_from_slice(b"\r\n");
    }
    out
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

/// Transmit for the tmux path: same 4096-byte chunking, but each chunk gets its
/// own passthrough envelope (tmux only forwards whole DCS sequences), and the
/// placement is virtual (`U=1`) so the actual pixels land where the placeholder
/// cells are printed. `q=2` silences both OK and error replies, which would
/// otherwise reach the pane as stray input.
fn tmux_transmit_bytes(png: &[u8], cols: usize, rows: usize, id: u32) -> Vec<u8> {
    let header = format!("a=T,U=1,q=2,f=100,i={id},c={cols},r={rows}");
    kitty_chunks(png, &header)
        .iter()
        .flat_map(|chunk| tmux_passthrough(chunk))
        .collect()
}

/// Unicode placeholder character carrying a virtual placement cell.
const PLACEHOLDER: char = '\u{10EEEE}';

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

/// `rows` lines of `cols` placeholder cells, each cell being the placeholder
/// character plus its row and column diacritic, with the image id carried in
/// the 24-bit foreground colour. Each line ends with a foreground reset and
/// `\r\n`, so printing the block advances the cursor by `rows` lines and every
/// row starts at column 0 — with a bare `\n` under raw mode each row would
/// start where the previous one ended, shearing the placeholder grid.
///
/// `cols`/`rows` are clamped to the diacritic table, which covers the display
/// box ([`MAX_COLS`] × [`MAX_ROWS`]).
fn placeholder_rows(cols: usize, rows: usize, id: u32) -> String {
    let id = normalize_image_id(id);
    let row_marks = &ROWCOLUMN_DIACRITICS[..rows.min(ROWCOLUMN_DIACRITICS.len())];
    let col_marks = &ROWCOLUMN_DIACRITICS[..cols.min(ROWCOLUMN_DIACRITICS.len())];
    let fg = format!(
        "\x1b[38;2;{};{};{}m",
        (id >> 16) & 0xFF,
        (id >> 8) & 0xFF,
        id & 0xFF
    );
    let mut out = String::with_capacity(row_marks.len() * (col_marks.len() * 9 + fg.len() + 6));
    for &row_mark in row_marks {
        out.push_str(&fg);
        for &col_mark in col_marks {
            out.push(PLACEHOLDER);
            out.push(row_mark);
            out.push(col_mark);
        }
        out.push_str("\x1b[39m\r\n");
    }
    out
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

/// 单一出口：按 `cap.mode` 分派出「传输 + 放置 + 光标推进」的完整字节序列。
///
/// `id` only matters in placeholder mode, where it ties the transmitted image
/// to the placeholder cells; it is masked to 24 bits.
pub fn image_print_bytes(
    cap: &ImageCap,
    png: &[u8],
    cols: usize,
    rows: usize,
    id: u32,
) -> Vec<u8> {
    match cap.mode {
        ImageMode::Direct => kitty_image_bytes(png, cols, rows),
        ImageMode::TmuxPlaceholder => {
            let id = normalize_image_id(id);
            let mut out = tmux_transmit_bytes(png, cols, rows, id);
            out.extend_from_slice(placeholder_rows(cols, rows, id).as_bytes());
            out
        }
    }
}

/// 从 url 加载图片并转成可传输的 ImageMeta：
/// - `data:image/...;base64,` — 内联 base64
/// - `http(s)://` — 下载（reqwest）
/// - 其他 — 本地路径（相对 cwd）
///
/// 解码 → 缩放（fit_cells）→ 编码 PNG。任何一步失败返回 None。
pub async fn load_image(url: &str, cwd: &Path, cap: &ImageCap) -> Option<ImageMeta> {    let bytes = fetch_bytes(url, cwd).await?;
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
    Some(ImageMeta {
        cols: cols as usize,
        rows: rows as usize,
        bytes: out,
    })
}

/// 按 url 类型取原始字节。
async fn fetch_bytes(url: &str, cwd: &Path) -> Option<Vec<u8>> {
    // CommonMark 角括号包裹的 URL（`![alt](<path with spaces>)`）剥壳，
    // 与渲染层的 key 保持一致。
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
        let client = reqwest::Client::new();
        let resp = client.get(url).send().await.ok()?;
        return resp.bytes().await.ok().map(|b| b.to_vec());
    }
    let path = Path::new(url);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    std::fs::read(path).ok()
}

/// 解码 `data:[mediatype][;base64],<data>`（仅支持 base64 变体）。
fn decode_data_url(head: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let comma = head.find(',')?;
    let (meta, data) = head.split_at(comma);
    if !meta.ends_with(";base64") {
        return None;
    }
    let b64 = &data[1..];
    let engine = base64::engine::general_purpose::STANDARD;
    engine.decode(b64).or_else(|_| {
        base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64)
    }).ok()
}

/// 从 markdown 文本提取图片 url（`![alt](url)`，url 不含空白）。
pub fn extract_image_urls(text: &str) -> Vec<String> {
    // 捕获后剥 `<>`（CommonMark 角括号包裹）：渲染层（rsmarkdown）同样
    // 剥壳，保持同一 key，否则加载缓存与渲染对不上。
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

/// macOS：剪贴板含图片时读出 PNG 字节（osascript `«class PNGf»`；
/// 非 macOS / 剪贴板无图片 / 任一步失败返回 None）。
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
    fn env_kitty_detects_supported_terminals() {
        assert!(env_kitty(Some("ghostty"), None));
        assert!(env_kitty(Some("WezTerm"), None));
        assert!(env_kitty(Some("kitty"), None));
        assert!(env_kitty(Some("konsole"), None));
        assert!(env_kitty(None, Some("xterm-kitty")));
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
        assert_eq!(parse_text_area_px(b"junk\x1b[4;25;120tmore"), Some((120, 25)));
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
        assert_eq!(cells_from_text_area(1512, 982, 0, 60), None, "divide by zero");
        assert_eq!(cells_from_text_area(1512, 982, 189, 0), None, "divide by zero");
        assert_eq!(cells_from_text_area(0, 0, 189, 60), None, "no pixels reported");
        // The old bug: the text-area size taken straight as the cell size.
        assert_eq!(cells_from_text_area(1512, 982, 1, 1), None);
        assert_eq!(cells_from_text_area(300, 982, 100, 60), None, "cell_w 3 < 4");
        assert_eq!(cells_from_text_area(1512, 300, 189, 60), None, "cell_h 5 < 6");
        assert_eq!(cells_from_text_area(6500, 1600, 100, 100), None, "cell_w 65 > 64");
        assert_eq!(cells_from_text_area(800, 12900, 100, 100), None, "cell_h 129 > 128");
    }

    #[test]
    fn cap_from_pairs_pixels_with_grid() {
        let full = b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c\x1b[4;982;1512t";
        assert_eq!(
            cap_from(Some(full), Some((189, 60)), ImageMode::Direct),
            ImageCap { cell_w: 8, cell_h: 16, mode: ImageMode::Direct }
        );
        // Unknown grid size → defaults, mode preserved.
        assert_eq!(
            cap_from(Some(full), None, ImageMode::TmuxPlaceholder),
            ImageCap { mode: ImageMode::TmuxPlaceholder, ..ImageCap::default_cells() }
        );
        // No 14t answer → defaults.
        assert_eq!(
            cap_from(Some(b"\x1b_Gi=31;OK\x1b\\"), Some((189, 60)), ImageMode::Direct),
            ImageCap::default_cells()
        );
        assert_eq!(cap_from(None, Some((189, 60)), ImageMode::Direct), ImageCap::default_cells());
    }

    #[test]
    fn fit_cells_regression_on_text_area_as_cell() {
        // Regression: the 14t numbers used as a cell size collapse everything.
        let bug = ImageCap { cell_w: 1512, cell_h: 982, mode: ImageMode::Direct };
        assert_eq!(fit_cells(800, 600, &bug, MAX_COLS, MAX_ROWS), (1, 1));
        let fixed = ImageCap { cell_w: 8, cell_h: 16, mode: ImageMode::Direct };
        assert_eq!(fit_cells(800, 600, &fixed, MAX_COLS, MAX_ROWS), (48, 18));
    }

    #[test]
    fn fit_cells_scales_to_fit_without_upscale() {
        let cap = ImageCap::default_cells();
        // 80×80 像素 = 10×2.5 cells；不放大。
        assert_eq!(fit_cells(80, 40, &cap, MAX_COLS, MAX_ROWS), (10, 3));
        // 超大图 → 缩到最大框（60×18 内，等比）。
        assert_eq!(fit_cells(8000, 6000, &cap, MAX_COLS, MAX_ROWS), (48, 18));
        // 小图不放大。
        assert_eq!(fit_cells(16, 16, &cap, MAX_COLS, MAX_ROWS), (2, 1));
        // 行高受限时按行反推。
        let (c, r) = fit_cells(4000, 4000, &cap, MAX_COLS, MAX_ROWS);
        assert_eq!(r, 18);
        assert_eq!(c, 36);
    }

    #[test]
    fn kitty_sequence_single_chunk() {
        // 小 payload：单块 m=0，含完整控制数据，末尾 rows 个 \r\n。
        let out = kitty_image_bytes(b"abc", 12, 4);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=100,q=1,c=12,r=4,C=1,m=0;"));
        assert!(s.ends_with("\r\n\r\n\r\n\r\n"));
        assert!(s.contains("\x1b\\"));
        assert_eq!(s.matches("\x1b\\").count(), 1);
    }

    #[test]
    fn kitty_sequence_chunks_at_4096() {
        // 每 4096 base64 字符 = 3072 字节。6000 字节 → 2 块。
        let png = vec![0u8; 6000];
        let out = kitty_image_bytes(&png, 10, 2);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("\x1b\\").count(), 2);
        assert!(s.contains("m=1;"), "首块 m=1");
        assert!(s.contains("m=0;"), "末块 m=0");
        let first = &s[s.find("m=1;").unwrap() + 4..];
        assert_eq!(first.find("\x1b\\").unwrap(), 4096, "首块 4096 字符");
        // 续块只带 m。
        let second_start = s.find("m=0;").unwrap();
        assert!(!s[second_start..].contains("a=T"), "续块不含控制数据");
        assert!(s.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn probe_plan_env_matrix() {
        // No tmux: unchanged behaviour, env_kitty short circuit intact.
        assert_eq!(
            probe_plan(false, Some("ghostty"), None),
            ProbePlan::Direct { env_kitty: true }
        );
        assert_eq!(
            probe_plan(false, Some("WezTerm"), None),
            ProbePlan::Direct { env_kitty: true }
        );
        assert_eq!(
            probe_plan(false, Some("Apple_Terminal"), Some("xterm-256color")),
            ProbePlan::Direct { env_kitty: false }
        );
        // tmux fronted by a placeholder-capable terminal → probe.
        assert_eq!(probe_plan(true, Some("ghostty"), None), ProbePlan::TmuxProbe);
        assert_eq!(probe_plan(true, Some("kitty"), None), ProbePlan::TmuxProbe);
        assert_eq!(probe_plan(true, None, Some("xterm-kitty")), ProbePlan::TmuxProbe);
        // No U=1 support outside → placeholder, no probe.
        assert_eq!(probe_plan(true, Some("WezTerm"), None), ProbePlan::TmuxUnsupported);
        assert_eq!(probe_plan(true, Some("konsole"), None), ProbePlan::TmuxUnsupported);
        assert_eq!(
            probe_plan(true, Some("Apple_Terminal"), Some("screen-256color")),
            ProbePlan::TmuxUnsupported
        );
        assert_eq!(probe_plan(true, None, None), ProbePlan::TmuxUnsupported);
    }

    #[test]
    fn graphics_query_ok_reads_passthrough_reply() {
        // The outer terminal's answer arrives unwrapped on the pane's stdin.
        assert!(graphics_query_ok(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;c\x1b[4;982;1512t"));
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
        assert_eq!(tmux_passthrough(b"plain"), b"\x1bPtmux;plain\x1b\\".to_vec());
    }

    #[test]
    fn tmux_transmit_wraps_every_chunk() {
        // 6000 bytes → 8000 base64 chars → 2 chunks, 2 envelopes.
        let png = vec![0u8; 6000];
        let out = tmux_transmit_bytes(&png, 10, 2, 0x01_0203);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s.matches("\x1bPtmux;").count(), 2, "one envelope per chunk");
        assert_eq!(s.matches("\x1b_G").count(), 2);
        assert_eq!(s.matches("\x1b\x1b_G").count(), 2, "every ESC in the body doubled");
        assert!(s.contains("\x1b\x1b_Ga=T,U=1,q=2,f=100,i=66051,c=10,r=2,m=1;"));
        assert!(s.contains("\x1b\x1b_Gm=0;"), "continuation chunk carries only m");
        assert!(s.ends_with("\x1b\\"));
        assert!(!s.contains('\n'), "transport alone never moves the cursor");
    }

    #[test]
    fn placeholder_rows_encode_grid_and_id() {
        let out = placeholder_rows(3, 2, 0x0A_0B0C);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "one line per row");
        for line in &lines {
            assert!(line.starts_with("\x1b[38;2;10;11;12m"), "id in the 24-bit fg");
            assert!(line.ends_with("\x1b[39m"), "fg reset at end of line");
            assert_eq!(
                line.chars().filter(|c| *c == PLACEHOLDER).count(),
                3,
                "one placeholder cell per column"
            );
        }
        // Diacritics are row-first then column, 0-based into kitty's table.
        assert!(lines[0].starts_with(&format!("\x1b[38;2;10;11;12m{PLACEHOLDER}\u{305}\u{305}")));
        assert!(lines[1].ends_with(&format!("{PLACEHOLDER}\u{30d}\u{30e}\x1b[39m")));
        assert_eq!(
            out.matches("\r\n").count(),
            2,
            "each row returns to column 0 and advances (raw mode)"
        );
    }

    #[test]
    fn placeholder_rows_clamps_and_normalizes_id() {
        // Beyond the diacritic table the block is clamped, never panics.
        let out = placeholder_rows(1000, 1000, 1);
        assert_eq!(out.lines().count(), ROWCOLUMN_DIACRITICS.len());
        // id 0 means "no id" to kitty; the high byte never survives the fg.
        assert!(placeholder_rows(1, 1, 0).starts_with("\x1b[38;2;0;0;1m"));
        assert!(placeholder_rows(1, 1, 0xFF00_0000).starts_with("\x1b[38;2;0;0;1m"));
        assert!(placeholder_rows(1, 1, 0x01FF_FFFF).starts_with("\x1b[38;2;255;255;255m"));
    }

    #[test]
    fn diacritics_table_matches_kitty() {
        assert_eq!(ROWCOLUMN_DIACRITICS.len(), MAX_COLS as usize);
        assert_eq!(ROWCOLUMN_DIACRITICS[0], '\u{305}', "index 0 encodes coordinate 0");
        assert_eq!(ROWCOLUMN_DIACRITICS[1], '\u{30d}');
        assert_eq!(ROWCOLUMN_DIACRITICS[59], '\u{615}');
        assert!(
            ROWCOLUMN_DIACRITICS.windows(2).all(|w| w[0] < w[1]),
            "kitty's table is codepoint-ordered and duplicate-free"
        );
    }

    #[test]
    fn image_print_bytes_dispatches_on_mode() {
        let direct = ImageCap::default_cells();
        assert_eq!(
            image_print_bytes(&direct, b"abc", 4, 2, 7),
            kitty_image_bytes(b"abc", 4, 2),
            "Direct mode is byte-identical to the plain kitty path"
        );
        let tmux = ImageCap { mode: ImageMode::TmuxPlaceholder, ..ImageCap::default_cells() };
        let out = image_print_bytes(&tmux, b"abc", 4, 2, 7);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bPtmux;"), "transmit first");
        assert!(s.contains("i=7,c=4,r=2"));
        assert!(s.contains(&format!("m{PLACEHOLDER}")), "placement follows");
        assert_eq!(s.matches("\r\n").count(), 2, "cursor advances rows lines");
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
            extract_image_urls("看 ![图](a.png) 和 ![b](https://x.com/i.png) 完"),
            vec!["a.png".to_string(), "https://x.com/i.png".to_string()]
        );
        // 角括号包裹的 URL（CommonMark `<...>`）剥壳，与渲染层 key 一致。
        assert_eq!(
            extract_image_urls("![图](</Users/x/Untitled-1.png>)"),
            vec!["/Users/x/Untitled-1.png".to_string()]
        );
        assert_eq!(extract_image_urls("无图片"), Vec::<String>::new());
        assert_eq!(extract_image_urls("![alt](has space.png)"), Vec::<String>::new());
    }

    #[test]
    fn load_image_from_data_url() {
        let cap = ImageCap::default_cells();
        let png = tiny_png();
        let url = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png));
        let meta = tokio::runtime::Runtime::new().unwrap().block_on(load_image(&url, Path::new("."), &cap));
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
        // file:// + 百分号编码（模型常把含空格路径写成 %20）。
        let url = format!("file://{}", path.display());
        let encoded = url.replace(' ', "%20");
        let bytes = runtime.block_on(fetch_bytes(&encoded, Path::new("/nonexistent")));
        assert_eq!(bytes, Some(b"\x89PNG\r\n\x1a\n".to_vec()), "file url decodes");
        // 相对 file 路径按 cwd 解析。
        let rel = runtime.block_on(fetch_bytes("sub/img.png", &tmp));
        assert_eq!(rel, None, "相对路径缺失时失败");
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("sub/img.png"), b"x").unwrap();
        let rel = runtime.block_on(fetch_bytes("sub/img.png", &tmp));
        assert_eq!(rel, Some(b"x".to_vec()), "相对路径按 cwd 解析");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_image_rejects_garbage() {
        let cap = ImageCap::default_cells();
        let url = "data:image/png;base64,AAAA".to_string();
        let meta = tokio::runtime::Runtime::new().unwrap().block_on(load_image(&url, Path::new("."), &cap));
        assert!(meta.is_none());
    }

    /// 4×2 纯色 PNG（测试用）。
    fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }
}
