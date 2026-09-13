//! What a file becomes on its way to Feishu: a route, and a form body.
//!
//! Both are pure and fixture-pinned, because both are wire formats nothing
//! else in this process can check. The body is hand-written rather than
//! reqwest's `multipart` feature: one page of RFC 7578 against a build feature
//! and the crates behind it, for two endpoints (ADR-0051 §3).
//!
//! Routing is Feishu's own table, from the upload endpoints' documented
//! `file_type` values (开发文档 · 服务端 API · 消息 · 图片/文件 · 上传图片,
//! 上传文件): `opus`, `mp4`, `pdf`, `doc`, `xls`, `ppt`, `stream` — and
//! nothing else, so `.docx` goes up as `doc` and an unknown extension as
//! `stream`.

/// Which endpoint takes the bytes, and therefore what comes back: an
/// `image_key` from one, a `file_key` from the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Image,
    File,
}

/// Where one file goes and what the message carrying it is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub endpoint: Endpoint,
    /// The type the endpoint is told: a file's `file_type`, and for a picture
    /// the `image_type`, which is always `message` — an avatar is not
    /// something a chat sends.
    pub file_type: &'static str,
    /// The `msg_type` of the message that carries the key.
    pub msg_type: &'static str,
}

/// Where `name`'s bytes belong.
///
/// A picture is decided by its bytes and everything else by its extension: a
/// screenshot saved as `.dat` is still a picture, while an `.mp4` is a video
/// because it is called one — Feishu asks for a `file_type` before it has
/// looked at anything.
pub fn route(name: &str, bytes: &[u8]) -> Route {
    if pictured(bytes) {
        return Route {
            endpoint: Endpoint::Image,
            file_type: "message",
            msg_type: "image",
        };
    }
    let (file_type, msg_type) = match extension(name).as_str() {
        "ogg" | "opus" => ("opus", "audio"),
        "mp4" => ("mp4", "media"),
        "pdf" => ("pdf", "file"),
        "doc" | "docx" => ("doc", "file"),
        "xls" | "xlsx" => ("xls", "file"),
        "ppt" | "pptx" => ("ppt", "file"),
        _ => ("stream", "file"),
    };
    Route {
        endpoint: Endpoint::File,
        file_type,
        msg_type,
    }
}

/// Whether these bytes open like a picture.
///
/// The magic numbers, not `bingo_pictures::sniffed`: that decodes the picture
/// and re-encodes an unusual one, and refuses anything over the cap a *model*
/// may be handed — so a 12 MB JPEG a person asked for would route as a plain
/// file, and a BMP would be decoded twice for a routing decision. Deciding
/// where the bytes go reads only the first few of them.
fn pictured(bytes: &[u8]) -> bool {
    let starts = |magic: &[u8]| bytes.starts_with(magic);
    starts(b"\x89PNG\r\n\x1a\n")
        || starts(b"\xff\xd8\xff")
        || starts(b"GIF87a")
        || starts(b"GIF89a")
        || starts(b"BM")
        || starts(b"II*\0")
        || starts(b"MM\0*")
        || (starts(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

/// The lower-cased extension of a name, without its dot.
fn extension(name: &str) -> String {
    std::path::Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// One `multipart/form-data` body (RFC 7578): the text fields in order, then
/// the file, then the closing boundary. Every line ends CRLF, which is the
/// half of this format a hand-written encoder gets wrong.
///
/// Returns the `Content-Type` header value beside the body, because the two
/// carry the same boundary and are only ever right together.
pub fn multipart(
    boundary: &str,
    fields: &[(&str, &str)],
    file: (&str, &str, &[u8]),
) -> (String, Vec<u8>) {
    let (field, name, bytes) = file;
    let mut body = Vec::with_capacity(bytes.len() + 512);
    for (key, value) in fields {
        push(&mut body, &format!("--{boundary}\r\n"));
        push(
            &mut body,
            &format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                header(key)
            ),
        );
        push(&mut body, &format!("{value}\r\n"));
    }
    push(&mut body, &format!("--{boundary}\r\n"));
    push(
        &mut body,
        &format!(
            "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\n",
            header(field),
            header(name)
        ),
    );
    push(&mut body, "Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(bytes);
    push(&mut body, &format!("\r\n--{boundary}--\r\n"));
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// A name as a header parameter can carry it. A quote or a newline in a file
/// name would end the parameter early and let the rest of it be read as a
/// header of its own, so neither survives the trip.
fn header(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\r' | '\n' => '_',
            other => other,
        })
        .collect()
}

fn push(body: &mut Vec<u8>, text: &str) {
    body.extend_from_slice(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\n and then some".to_vec()
    }

    #[test]
    fn every_route_is_the_one_feishus_own_table_names() {
        let file = |name: &str| route(name, b"not a picture");
        assert_eq!(
            route("shot.dat", &png()),
            Route {
                endpoint: Endpoint::Image,
                file_type: "message",
                msg_type: "image",
            },
            "the bytes say picture whatever the name says"
        );
        for (name, file_type, msg_type) in [
            ("note.ogg", "opus", "audio"),
            ("note.OPUS", "opus", "audio"),
            ("clip.mp4", "mp4", "media"),
            ("report.pdf", "pdf", "file"),
            ("report.doc", "doc", "file"),
            ("report.docx", "doc", "file"),
            ("sheet.xls", "xls", "file"),
            ("sheet.xlsx", "xls", "file"),
            ("deck.ppt", "ppt", "file"),
            ("deck.pptx", "ppt", "file"),
            ("archive.tar.gz", "stream", "file"),
            ("README", "stream", "file"),
        ] {
            assert_eq!(
                file(name),
                Route {
                    endpoint: Endpoint::File,
                    file_type,
                    msg_type,
                },
                "{name}"
            );
        }
    }

    #[test]
    fn a_picture_is_recognised_by_its_first_bytes_in_every_format_the_endpoint_takes() {
        let picture = |bytes: &[u8]| route("x.bin", bytes).endpoint;
        for bytes in [
            png(),
            b"\xff\xd8\xff\xe0 jfif".to_vec(),
            b"GIF89a....".to_vec(),
            b"GIF87a....".to_vec(),
            b"BM....".to_vec(),
            b"II*\0....".to_vec(),
            b"MM\0*....".to_vec(),
            b"RIFF\x04\0\0\0WEBPVP8 ".to_vec(),
        ] {
            assert_eq!(picture(&bytes), Endpoint::Image, "{bytes:?}");
        }
        assert_eq!(picture(b"RIFF\x04\0\0\0WAVEfmt "), Endpoint::File);
        assert_eq!(picture(b""), Endpoint::File);
    }

    /// The body is what the other end parses; a byte of it is a bug.
    #[test]
    fn the_body_is_rfc_7578_to_the_byte() {
        let (content_type, body) = multipart(
            "----bingo1",
            &[("file_type", "stream"), ("file_name", "notes.txt")],
            ("file", "notes.txt", b"by the chat\n"),
        );
        assert_eq!(
            content_type, "multipart/form-data; boundary=----bingo1",
            "the header carries the body's own boundary"
        );
        assert_eq!(
            String::from_utf8(body).expect("the fixture is text"),
            concat!(
                "------bingo1\r\n",
                "Content-Disposition: form-data; name=\"file_type\"\r\n\r\n",
                "stream\r\n",
                "------bingo1\r\n",
                "Content-Disposition: form-data; name=\"file_name\"\r\n\r\n",
                "notes.txt\r\n",
                "------bingo1\r\n",
                "Content-Disposition: form-data; name=\"file\"; filename=\"notes.txt\"\r\n",
                "Content-Type: application/octet-stream\r\n\r\n",
                "by the chat\n\r\n",
                "------bingo1--\r\n",
            )
        );
    }

    #[test]
    fn the_bytes_go_through_untouched_and_a_hostile_name_cannot_forge_a_header() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let (_, body) = multipart("b", &[], ("file", "a\".txt\r\nX-Evil: 1\r\n\r\n", &bytes));
        let text = String::from_utf8_lossy(&body).into_owned();
        assert!(
            text.contains("filename=\"a_.txt__X-Evil: 1____\""),
            "the quotes and the line breaks are gone: {text}"
        );
        let opened = body
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("the part's blank line");
        assert_eq!(
            &body[opened + 4..opened + 4 + bytes.len()],
            &bytes[..],
            "every byte value survives the form"
        );
    }
}
