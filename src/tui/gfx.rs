//! 终端图片显示：kitty graphics protocol 能力检测 + 序列构建 + 图片加载。
//!
//! 只做 kitty 协议（Ghostty/kitty/WezTerm/Konsole 均支持）；其余终端
//! 由渲染层显示 `#[image]` 占位。图片以 PNG 传输（协议只认 PNG/RGB/RGBA），
//! 传输前按 cell 尺寸缩放到目标单元格尺寸。

use std::path::Path;

/// 最大显示宽度（单元格列数）。
pub const MAX_COLS: u32 = 60;
/// 最大显示高度（单元格行数）。
pub const MAX_ROWS: u32 = 18;
/// 单个图片文件大小上限。
const MAX_BYTES: usize = 10 * 1024 * 1024;
/// 图片像素尺寸上限（防止超大图撑爆解码与传输）。
const MAX_DIM: u32 = 16_000;

/// kitty 协议图片能力（含探测到的 cell 尺寸）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCap {
    /// 单个字符单元格的像素宽。
    pub cell_w: u32,
    /// 单个字符单元格的像素高。
    pub cell_h: u32,
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
        Self { cell_w: 8, cell_h: 16 }
    }
}

/// 检测终端是否支持 kitty graphics protocol（及 cell 尺寸）。
///
/// 快速路径：`TERM_PROGRAM=ghostty/WezTerm/kitty/konsole` 或
/// `TERM=xterm-kitty` 直接判定支持；否则向终端发起协议查询
/// （`a=q` 查询动作 + DA + 14t 像素尺寸），读到 `\x1b_Gi=31;OK`
/// 即支持。需在进 raw mode / 全屏前调用。
pub async fn detect_image_cap() -> Option<ImageCap> {
    let env_kitty = env_supports_kitty();
    let buf = crate::tui::theme::Theme::query_terminal(
        &[b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c\x1b[14t"],
        std::time::Duration::from_millis(400),
    )
    .await;
    if !env_kitty && !buf.as_deref().is_some_and(graphics_query_ok) {
        return None;
    }
    let (cell_w, cell_h) = buf
        .as_deref()
        .and_then(parse_text_area_px)
        .unwrap_or((ImageCap::default_cells().cell_w, ImageCap::default_cells().cell_h));
    Some(ImageCap { cell_w, cell_h })
}

/// 从环境变量判断 kitty 协议支持（纯函数，便于测试）。
pub fn env_kitty(term_program: Option<&str>, term: Option<&str>) -> bool {
    match term_program {
        Some("ghostty") | Some("WezTerm") | Some("kitty") | Some("konsole") => true,
        _ => term == Some("xterm-kitty"),
    }
}

fn env_supports_kitty() -> bool {
    let program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    env_kitty(program.as_deref(), term.as_deref())
}

/// 查询响应中是否含 kitty 图形协议 OK 应答（`\x1b_Gi=31;OK`）。
fn graphics_query_ok(buf: &[u8]) -> bool {
    buf.windows(b"\x1b_Gi=31;OK".len())
        .any(|w| w == b"\x1b_Gi=31;OK")
}

/// 解析 `\x1b[14t` 响应（`CSI 4 ; height ; width t`）为 (宽, 高) 像素。
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

/// 构建 kitty 传输+放置序列：base64 按 4096 字节分块（协议上限），
/// 首块携带完整控制数据（`a=T` 传输并显示、PNG、静默 OK 应答、
/// 不移动光标），续块只带 `m`。末尾追加 `rows` 个换行推进光标
/// （`C=1` 放置不移动光标，换行负责把光标移到图片块之后）。
pub fn kitty_image_bytes(png: &[u8], cols: usize, rows: usize) -> Vec<u8> {
    use base64::Engine;
    const CHUNK: usize = 4096;
    let b64 = base64::engine::general_purpose::STANDARD.encode(png);
    let bytes = b64.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / CHUNK * 8 + 32 + rows);
    let mut start = 0usize;
    loop {
        let end = (start + CHUNK).min(bytes.len());
        let more = end < bytes.len();
        let header = if start == 0 {
            format!("a=T,f=100,q=1,c={cols},r={rows},C=1,m={}", u8::from(more))
        } else {
            format!("m={}", u8::from(more))
        };
        out.extend_from_slice(b"\x1b_G");
        out.extend_from_slice(header.as_bytes());
        out.push(b';');
        out.extend_from_slice(&bytes[start..end]);
        out.extend_from_slice(b"\x1b\\");
        start = end;
        if !more {
            break;
        }
    }
    out.resize(out.len() + rows, b'\n');
    out
}

/// 从 url 加载图片并转成可传输的 ImageMeta：
/// - `data:image/...;base64,` — 内联 base64
/// - `http(s)://` — 下载（reqwest）
/// - 其他 — 本地路径（相对 cwd）
///
/// 解码 → 缩放（fit_cells）→ 编码 PNG。任何一步失败返回 None。
pub async fn load_image(url: &str, cwd: &Path, cap: &ImageCap) -> Option<ImageMeta> {
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
    Some(ImageMeta {
        cols: cols as usize,
        rows: rows as usize,
        bytes: out,
    })
}

/// 按 url 类型取原始字节。
async fn fetch_bytes(url: &str, cwd: &Path) -> Option<Vec<u8>> {
    if let Some(head) = url.strip_prefix("data:") {
        return decode_data_url(head);
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
    let Ok(re) = regex::Regex::new(r"!\[[^\]]*\]\(([^)\s]+)\)") else {
        return Vec::new();
    };
    re.captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
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
        assert_eq!(parse_text_area_px(b"\x1b[4;40;80t"), Some((80, 40)));
        assert_eq!(parse_text_area_px(b"junk\x1b[4;25;120tmore"), Some((120, 25)));
        assert_eq!(parse_text_area_px(b"\x1b[4;0;0t"), None);
        assert_eq!(parse_text_area_px(b"no response"), None);
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
        // 小 payload：单块 m=0，含完整控制数据，末尾 rows 个换行。
        let out = kitty_image_bytes(b"abc", 12, 4);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b_Ga=T,f=100,q=1,c=12,r=4,C=1,m=0;"));
        assert!(s.ends_with("\n\n\n\n"));
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
