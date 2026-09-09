//! What a message carried, fetched (ADR-0040, ADR-0051 §2).
//!
//! Parsing hands over keys; this is where they become bytes. A picture
//! becomes the one `Image` the journal keeps, its type read off the bytes
//! because a chat carries whatever phones and screenshot keys produce and a
//! BMP goes as PNG (ADR-0041 §2). Everything else lands on disk under the
//! message that carried it and becomes a path in the words: the model reads
//! it with the fs tool it already has, and no kernel type has to learn what a
//! document is.
//!
//! Nothing here is fatal. A resource that will not fetch — no `im:resource`
//! scope, bytes no decoder reads, a disk that refuses it — is dropped with a
//! warning and the words still go: somebody who typed a caption is not
//! silenced by the file beside it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bingo_pictures::cache::{DAYS, fresh};
use bingo_sdk::Image;

use super::api::{Api, Binary};
use super::content::{Kind, Resource, fenced};

/// What one message's resources came to.
#[derive(Debug, Default)]
pub struct Fetched {
    /// The pictures, in the order they were placed, minus any that failed.
    pub images: Vec<Image>,
    /// One line per attachment saying where it landed, and the words of a
    /// small text file under its own line.
    pub lines: Vec<String>,
}

/// The most one attachment is kept: Feishu's own upload limit, so a file this
/// surface refuses is one it could not have sent back either.
const MOST: usize = 30 * 1024 * 1024;

/// A text file up to this big goes into the words as well as onto disk. It is
/// what the person meant to say; making the model read back a path it was
/// handed one line earlier is a round trip for nothing.
const INLINE: usize = 100 * 1024;

/// Content types whose bytes are words, whatever the name says.
const READABLE: [&str; 2] = ["text/plain", "text/markdown"];

/// Names whose words are read, whatever the content type says.
const READS: [&str; 3] = [".txt", ".md", ".markdown"];

/// A name long enough for anything a person types, short enough that the path
/// still fits under the shortest temporary directory a CI runner has.
const NAME: usize = 120;

/// Characters no name may hold: the two separators, and the ones Windows
/// refuses. The strictest of the three platforms decides for all of them — a
/// name that writes on one machine and not another is a bug that only ever
/// shows in the release matrix.
const FORBIDDEN: [char; 9] = ['/', '\\', '<', '>', ':', '"', '|', '?', '*'];

/// Everything `resources` names, fetched into what the surface can use.
pub async fn fetch(api: &Api, dir: &Path, resources: &[Resource]) -> Fetched {
    let mut fetched = Fetched::default();
    if resources
        .iter()
        .any(|resource| resource.kind != Kind::Picture)
    {
        swept(dir);
    }
    for resource in resources {
        match resource.kind {
            Kind::Picture => match picture(api, resource).await {
                Ok(image) => fetched.images.push(image),
                Err(why) => tracing::warn!(key = %resource.key, %why, "a picture was dropped"),
            },
            _ => match saved(api, dir, resource).await {
                Ok(lines) => fetched.lines.extend(lines),
                Err(why) => tracing::warn!(key = %resource.key, %why, "an attachment was dropped"),
            },
        }
    }
    fetched
}

pub fn resource_path(resource: &Resource, kind: &str) -> String {
    format!(
        "/open-apis/im/v1/messages/{}/resources/{}?type={kind}",
        resource.message, resource.key
    )
}

async fn picture(api: &Api, resource: &Resource) -> Result<Image, String> {
    let binary = fetched(api, resource).await?;
    bingo_pictures::sniffed(&binary.bytes).map_err(|e| e.to_string())
}

/// One attachment on disk, and the words that point at it.
async fn saved(api: &Api, dir: &Path, resource: &Resource) -> Result<Vec<String>, String> {
    let binary = fetched(api, resource).await?;
    let name = filed(resource);
    if binary.bytes.len() > MOST {
        return Ok(vec![oversized(&name, binary.bytes.len())]);
    }
    let path = written(&dir.join(&resource.message), &name, &binary.bytes)
        .map_err(|error| error.to_string())?;
    let mut lines = vec![format!("[file: {name} → {}]", path.display())];
    lines.extend(readable(&binary, &name));
    Ok(lines)
}

/// The bytes, through whichever `type` the platform will serve them under.
///
/// A voice note or a video answers as `audio` or `media` from most clients and
/// only as `file` from some, so the kind's own type is asked for first and
/// `file` is the fallback. The last refusal is what is reported: it is the one
/// that ran out of ways to ask.
async fn fetched(api: &Api, resource: &Resource) -> Result<Binary, String> {
    let mut refused = String::from("no way to ask for it");
    for kind in types(&resource.kind) {
        match api.get_bytes(&resource_path(resource, kind)).await {
            Ok(binary) => return Ok(binary),
            Err(why) => refused = why.to_string(),
        }
    }
    Err(refused)
}

fn types(kind: &Kind) -> &'static [&'static str] {
    match kind {
        Kind::Picture => &["image"],
        Kind::File { .. } => &["file"],
        Kind::Audio => &["audio", "file"],
        Kind::Video { .. } => &["media", "file"],
    }
}

/// What the attachment is written as: the platform's own name, reduced to a
/// name, else what it was fetched by. Feishu records a voice note as opus —
/// its upload endpoint takes no other audio format — and gives it no name.
fn filed(resource: &Resource) -> String {
    let given = match &resource.kind {
        Kind::File { name } | Kind::Video { name } => safe(name),
        Kind::Audio | Kind::Picture => String::new(),
    };
    if !given.is_empty() {
        return given;
    }
    let key = safe(&resource.key);
    match resource.kind {
        Kind::Audio => format!("{key}.opus"),
        Kind::Video { .. } => format!("video_{key}"),
        Kind::File { .. } => format!("file_{key}"),
        Kind::Picture => format!("image_{key}"),
    }
}

/// A name this machine will take and nothing else: the last segment of
/// whatever the platform called it, with every separator, device character and
/// control character replaced. A file cannot leave the directory of the
/// message that carried it, however it was named.
fn safe(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|c| match c.is_control() || FORBIDDEN.contains(&c) {
            true => '_',
            false => c,
        })
        .collect();
    match cleaned.trim().trim_end_matches('.').trim() {
        "" | "." | ".." => String::new(),
        name => cut(name),
    }
}

/// Shortened to fit, with its extension kept: what the extension says a file
/// is decides whether its words go in the message as well as on disk.
fn cut(name: &str) -> String {
    if name.len() <= NAME {
        return name.to_string();
    }
    let tail = match name.rsplit_once('.') {
        Some((_, extension)) if extension.len() <= 16 => format!(".{extension}"),
        _ => String::new(),
    };
    let mut end = NAME.saturating_sub(tail.len());
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{tail}", &name[..end])
}

/// Through a temporary name and a rename, which is what makes two bingos
/// sharing the directory safe: the file appears whole or not at all.
fn written(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let temporary = dir.join(part());
    std::fs::write(&temporary, bytes)?;
    let path = dir.join(name);
    std::fs::rename(&temporary, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })?;
    // A path in a transcript is a path on this machine (ADR-0051): an absolute
    // one, because the model's cwd is not this surface's business.
    Ok(std::path::absolute(&path).unwrap_or(path))
}

/// The name one write uses before its rename: this process, and a number no
/// other write in it repeats.
fn part() -> String {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let n = WRITES.fetch_add(1, Ordering::Relaxed);
    format!(".{}.{n}.part", std::process::id())
}

/// A file past the cap is named and not kept: what a person needs to know is
/// that it arrived and did not land, and a chat can fill a disk in an evening.
fn oversized(name: &str, size: usize) -> String {
    format!("[file: {name} — not kept: {size} bytes is over the {MOST}-byte limit]")
}

/// The words in a small text file, fenced under the line that named it.
fn readable(binary: &Binary, name: &str) -> Option<String> {
    let lowered = name.to_lowercase();
    let words = READS.iter().any(|end| lowered.ends_with(end))
        || READABLE.contains(&binary.content_type.as_str());
    if !words || binary.bytes.len() > INLINE {
        return None;
    }
    Some(fenced("", std::str::from_utf8(&binary.bytes).ok()?))
}

/// Entries older than a fortnight, gone, swept on the way in.
///
/// Nothing else ever walks this directory, and a chat is a firehose. When an
/// entry was written is its own modification time and nothing beside it — a
/// sidecar saying the same thing is a second fact to keep in step
/// (`bingo_pictures::cache`). A sweep that cannot read the directory is not a
/// reason to drop the file that is arriving.
fn swept(dir: &Path) {
    let ttl = Duration::from_secs(DAYS * 24 * 60 * 60);
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let fresh = entry
            .metadata()
            .and_then(|at| at.modified())
            .map(|at| fresh(at, now, ttl))
            .unwrap_or(true);
        if fresh {
            continue;
        }
        let _ = match entry.file_type().map(|kind| kind.is_dir()) {
            Ok(true) => std::fs::remove_dir_all(entry.path()),
            _ => std::fs::remove_file(entry.path()),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_pictures::testing::ImageFormat;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn signed_in(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 0, "tenant_access_token": "t-1", "expire": 7200,
            })))
            .mount(server)
            .await;
    }

    async fn api(server: &MockServer) -> Api {
        signed_in(server).await;
        Api::new(server.uri(), "cli_1", "secret")
    }

    fn resource(key: &str, kind: Kind) -> Resource {
        Resource {
            message: "om_1".into(),
            key: key.into(),
            kind,
        }
    }

    fn file(name: &str) -> Kind {
        Kind::File { name: name.into() }
    }

    /// The endpoint, scripted for one key under one `type`.
    async fn served(server: &MockServer, key: &str, kind: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(format!(
                "/open-apis/im/v1/messages/om_1/resources/{key}"
            )))
            .and(query_param("type", kind))
            .and(header("authorization", "Bearer t-1"))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn serving(server: &MockServer, key: &str, kind: &str, content: &str, bytes: Vec<u8>) {
        served(
            server,
            key,
            kind,
            ResponseTemplate::new(200)
                .insert_header("content-type", content)
                .set_body_bytes(bytes),
        )
        .await;
    }

    fn drawn(format: ImageFormat) -> Vec<u8> {
        bingo_pictures::testing::drawn(4, 3, format)
    }

    /// The saved file a `[file: … → …]` line points at.
    fn pointed_at(line: &str) -> PathBuf {
        let (_, tail) = line.rsplit_once(" → ").expect("an arrow: {line}");
        PathBuf::from(tail.trim_end_matches(']'))
    }

    #[tokio::test]
    async fn one_of_each_kind_lands_where_the_words_say_it_did() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        serving(
            &server,
            "img_a",
            "image",
            "image/png",
            drawn(ImageFormat::Png),
        )
        .await;
        serving(
            &server,
            "file_a",
            "file",
            "application/pdf",
            b"%PDF-1.4".to_vec(),
        )
        .await;
        serving(&server, "audio_a", "audio", "audio/opus", b"OggS".to_vec()).await;
        serving(
            &server,
            "media_a",
            "media",
            "video/mp4",
            b"ftypmp4".to_vec(),
        )
        .await;
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("img_a", Kind::Picture),
                resource("file_a", file("report.pdf")),
                resource("audio_a", Kind::Audio),
                resource(
                    "media_a",
                    Kind::Video {
                        name: "demo.mp4".into(),
                    },
                ),
            ],
        )
        .await;
        assert_eq!(
            fetched.images.len(),
            1,
            "a picture joins the ask, it does not land on disk"
        );
        let shown = fetched.lines.join("\n").replace(
            dir.path().to_str().expect("a utf-8 temporary directory"),
            "<files>",
        );
        insta::assert_snapshot!("feishu-attachments", shown);
        for line in &fetched.lines {
            let at = pointed_at(line);
            assert!(at.is_absolute(), "{at:?}");
            assert!(at.exists(), "{at:?}");
            assert_eq!(at.parent(), Some(dir.path().join("om_1").as_path()));
        }
    }

    /// A name is a name, never a path: a file called `../../etc/passwd` lands
    /// beside the others or nowhere at all.
    #[tokio::test]
    async fn a_hostile_name_cannot_leave_the_message_it_arrived_in() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        for key in ["file_a", "file_b", "file_c"] {
            serving(
                &server,
                key,
                "file",
                "application/octet-stream",
                b"x".to_vec(),
            )
            .await;
        }
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("file_a", file("../../etc/passwd")),
                resource("file_b", file("a/b.txt")),
                resource("file_c", file("..")),
            ],
        )
        .await;
        let landed: Vec<PathBuf> = fetched
            .lines
            .iter()
            .filter(|line| line.starts_with("[file: "))
            .map(|line| pointed_at(line))
            .collect();
        let inside = dir.path().join("om_1");
        for at in &landed {
            assert_eq!(at.parent(), Some(inside.as_path()), "{at:?}");
        }
        let names: Vec<&str> = landed
            .iter()
            .filter_map(|at| at.file_name()?.to_str())
            .collect();
        assert_eq!(names, ["passwd", "b.txt", "file_file_c"]);
        assert!(!dir.path().join("etc").exists());
    }

    /// Some clients upload a voice note or a video that the platform will only
    /// hand back as a plain file.
    #[tokio::test]
    async fn a_voice_note_that_is_not_served_as_audio_is_served_as_a_file() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        served(&server, "audio_a", "audio", ResponseTemplate::new(404)).await;
        serving(&server, "audio_a", "file", "audio/opus", b"OggS".to_vec()).await;
        let fetched = fetch(&api, dir.path(), &[resource("audio_a", Kind::Audio)]).await;
        assert_eq!(fetched.lines.len(), 1, "{:?}", fetched.lines);
        let at = pointed_at(&fetched.lines[0]);
        assert_eq!(
            at.file_name().and_then(|n| n.to_str()),
            Some("audio_a.opus")
        );
        assert_eq!(std::fs::read(&at).expect("the file"), b"OggS");
    }

    #[tokio::test]
    async fn a_refused_fetch_costs_the_attachment_and_not_the_words() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        served(&server, "file_gone", "file", ResponseTemplate::new(403)).await;
        serving(&server, "file_b", "file", "text/plain", b"kept".to_vec()).await;
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("file_gone", file("secret.pdf")),
                resource("file_b", file("notes.txt")),
            ],
        )
        .await;
        assert_eq!(
            fetched.lines.len(),
            2,
            "the one that arrived, and its words: {:?}",
            fetched.lines
        );
        assert!(fetched.lines[0].starts_with("[file: notes.txt → "));
        assert_eq!(fetched.lines[1], "```\nkept\n```");
    }

    #[tokio::test]
    async fn a_small_text_file_is_read_out_and_a_bigger_one_is_only_pointed_at() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        let long = "x".repeat(INLINE + 1);
        serving(&server, "file_a", "file", "text/markdown", b"# hi".to_vec()).await;
        serving(
            &server,
            "file_b",
            "file",
            "text/plain",
            long.clone().into_bytes(),
        )
        .await;
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("file_a", file("notes.md")),
                resource("file_b", file("huge.txt")),
            ],
        )
        .await;
        assert_eq!(fetched.lines.len(), 3, "{:?}", fetched.lines);
        assert_eq!(fetched.lines[1], "```\n# hi\n```");
        assert!(fetched.lines[2].starts_with("[file: huge.txt → "));
        assert_eq!(
            std::fs::read_to_string(pointed_at(&fetched.lines[2])).expect("the file"),
            long,
            "it is still on disk in full; only the words are capped"
        );
    }

    /// A chat can fill a disk in an evening, so the cap is a refusal and the
    /// refusal is in the words.
    #[tokio::test]
    async fn a_file_over_the_cap_is_named_and_not_kept() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        serving(
            &server,
            "file_a",
            "file",
            "application/zip",
            vec![b'x'; MOST + 1],
        )
        .await;
        let fetched = fetch(&api, dir.path(), &[resource("file_a", file("dump.zip"))]).await;
        assert_eq!(
            fetched.lines,
            vec![format!(
                "[file: dump.zip — not kept: {} bytes is over the {MOST}-byte limit]",
                MOST + 1
            )]
        );
        assert!(!dir.path().join("om_1").join("dump.zip").exists());
    }

    /// The sweep, on the way in: nothing else ever walks this directory.
    #[tokio::test]
    async fn what_a_fortnight_has_passed_over_is_swept_as_the_next_file_lands() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        serving(&server, "file_a", "file", "text/plain", b"new".to_vec()).await;
        let stale = dir.path().join("om_old");
        let fresh = dir.path().join("om_new");
        std::fs::write(&stale, b"old").expect("an entry");
        std::fs::write(&fresh, b"new").expect("an entry");
        aged(&stale, Duration::from_secs((DAYS + 1) * 24 * 60 * 60));
        fetch(&api, dir.path(), &[resource("file_a", file("a.txt"))]).await;
        assert!(!stale.exists(), "past its fortnight");
        assert!(fresh.exists(), "and nothing else touched");
    }

    /// Set an entry's modification time back, which is how a test makes one old
    /// without a sleep and without pinning a clock.
    fn aged(path: &Path, age: Duration) {
        let file = std::fs::File::options()
            .write(true)
            .open(path)
            .expect("the entry");
        file.set_modified(SystemTime::now() - age)
            .expect("a stamp this machine takes");
    }

    #[tokio::test]
    async fn pictures_are_fetched_in_order_and_a_refused_one_is_dropped() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        let png = drawn(ImageFormat::Png);
        serving(&server, "img_a", "image", "image/png", png.clone()).await;
        served(&server, "img_gone", "image", ResponseTemplate::new(404)).await;
        serving(
            &server,
            "img_b",
            "image",
            "image/png; charset=binary",
            drawn(ImageFormat::Jpeg),
        )
        .await;
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("img_a", Kind::Picture),
                resource("img_gone", Kind::Picture),
                resource("img_b", Kind::Picture),
            ],
        )
        .await;
        let types: Vec<&str> = fetched
            .images
            .iter()
            .map(|image| image.media_type.as_str())
            .collect();
        assert_eq!(
            types,
            ["image/png", "image/jpeg"],
            "the header said png for both; the bytes did not"
        );
        assert_eq!(
            fetched.images[0],
            Image::from_bytes("image/png", &png).expect("within the cap"),
            "a type the table takes is the bytes as they came"
        );
        assert!(fetched.lines.is_empty(), "a picture is not a path");
    }

    /// A screenshot off a Windows phone, a scan, a sticker: a chat carries more
    /// than the four types a provider takes, and the journal keeps a type that
    /// replays (ADR-0041 §2).
    #[tokio::test]
    async fn a_type_the_table_refuses_arrives_as_png() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        serving(
            &server,
            "img_bmp",
            "image",
            "image/bmp",
            drawn(ImageFormat::Bmp),
        )
        .await;
        serving(
            &server,
            "img_tiff",
            "image",
            "image/tiff",
            drawn(ImageFormat::Tiff),
        )
        .await;
        let fetched = fetch(
            &api,
            dir.path(),
            &[
                resource("img_bmp", Kind::Picture),
                resource("img_tiff", Kind::Picture),
            ],
        )
        .await;
        let types: Vec<&str> = fetched
            .images
            .iter()
            .map(|image| image.media_type.as_str())
            .collect();
        assert_eq!(types, ["image/png", "image/png"]);
    }

    #[tokio::test]
    async fn bytes_no_decoder_reads_are_dropped_whatever_the_header_says() {
        let server = MockServer::start().await;
        let api = api(&server).await;
        let dir = tempfile::tempdir().expect("a directory");
        serving(
            &server,
            "img_heic",
            "image",
            "image/png",
            b"not a picture at all".to_vec(),
        )
        .await;
        let fetched = fetch(&api, dir.path(), &[resource("img_heic", Kind::Picture)]).await;
        assert!(fetched.images.is_empty());
    }

    #[test]
    fn a_name_is_cut_to_fit_and_keeps_what_says_what_it_is() {
        let long = format!("{}.md", "n".repeat(300));
        let cut = safe(&long);
        assert!(cut.len() <= NAME, "{}", cut.len());
        assert!(cut.ends_with(".md"), "{cut}");
        assert_eq!(safe("a\u{0}b<c>d:e\"f|g?h*i"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(safe("  spaced.txt  "), "spaced.txt");
        assert_eq!(safe("."), "");
    }
}
