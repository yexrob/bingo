//! A picture read in from where it lives.
//!
//! A path is read off this machine's disk; a URL is fetched *by this machine*
//! and never handed to a provider as a URL (ADR-0041 §3), so the address the
//! request comes from is the person's and a picture behind a private name
//! still arrives. Both are bounded before they are held: the journal's cap is
//! checked on what the source says its size is, and again on what it actually
//! sends, so neither a huge file nor a lying server fills this process.
//!
//! A URL is fetched once and kept ([`crate::cache`]): a picture behind an
//! address does not change because a transcript was redrawn or a session
//! resumed. A path is not cached — the file *is* the cache, and a copy of it
//! under the data directory would be a second one to keep in step.

use std::path::Path;
use std::time::Duration;

use bingo_sdk::{Image, ImageError};
use futures::StreamExt;

use crate::accepted::sniffed;
use crate::cache::Cache;
use crate::{PictureError, Source};

/// How long a remote read may take, start to finish. A picture is beside a
/// sentence somebody is waiting to send, so the wait is bounded; it is
/// generous because the machine this runs on is not the machine it was
/// written on, and a phone tethering is still a network.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The picture `source` names, in a type a provider accepts. `cache` is where
/// a fetched one is kept and looked for; `None` for a caller that keeps none.
pub async fn load(source: &Source, cache: Option<&Cache>) -> Result<Image, PictureError> {
    let bytes = match source {
        Source::Path(path) => read(path)?,
        Source::Url(url) => remote(url, cache).await?,
    };
    sniffed(&bytes)
}

/// A URL out of the cache where a young enough copy is in it, and off the
/// network where it is not — and then kept, so the next reading of the same
/// address costs nothing.
///
/// What is kept is what the server sent, not what a decoder made of it: the
/// bytes are the fact, and everything else about the picture is derived from
/// them by the same code on a hit as on a miss.
async fn remote(url: &str, cache: Option<&Cache>) -> Result<Vec<u8>, PictureError> {
    let Some(cache) = cache else {
        return fetch(url).await;
    };
    if let Some(kept) = cache.hit(url) {
        return Ok(kept);
    }
    let bytes = fetch(url).await?;
    cache.keep(url, &bytes);
    Ok(bytes)
}

/// A path off this machine's disk, its size read before its bytes are: a file
/// too large to send is refused without being held in memory first.
fn read(path: &Path) -> Result<Vec<u8>, PictureError> {
    refuse_over(std::fs::metadata(path)?.len())?;
    Ok(std::fs::read(path)?)
}

/// A URL this machine fetches. `Content-Length` is refused before a byte of
/// the body is asked for, and the body is cut at the cap as it arrives, so a
/// server that understates its length is stopped where it passes the limit
/// rather than after it has been read whole.
async fn fetch(url: &str) -> Result<Vec<u8>, PictureError> {
    let response = client()?
        .get(url)
        .send()
        .await
        .map_err(shorn)?
        .error_for_status()
        .map_err(shorn)?;
    if let Some(length) = response.content_length() {
        refuse_over(length)?;
    }
    let mut body = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        bytes.extend_from_slice(&chunk.map_err(shorn)?);
        refuse_over(bytes.len() as u64)?;
    }
    Ok(bytes)
}

fn client() -> Result<reqwest::Client, PictureError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(shorn)
}

/// The URL out of a transport error's own message: the caller is naming the
/// source already, and a notice that says it twice reads like a stutter.
fn shorn(error: reqwest::Error) -> PictureError {
    PictureError::Unfetchable(error.without_url())
}

/// The journal's cap (ADR-0040), spelled against a size known before the
/// bytes are. It is `Image`'s limit and its own words: one table, one error.
fn refuse_over(size: u64) -> Result<(), PictureError> {
    let bytes = usize::try_from(size).unwrap_or(usize::MAX);
    match bytes > Image::MAX_BYTES {
        true => Err(PictureError::Refused(ImageError::TooLarge {
            bytes,
            max: Image::MAX_BYTES,
        })),
        false => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::png_size;
    use crate::testing::{ImageFormat, drawn};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn wrote(dir: &Path, name: &str, bytes: &[u8]) -> Source {
        let file = dir.join(name);
        std::fs::write(&file, bytes).expect("a picture on disk");
        Source::Path(file)
    }

    /// A picture served at `/shot`, and the URL that reaches it.
    async fn serving(server: &MockServer, response: ResponseTemplate) -> Source {
        Mock::given(method("GET"))
            .and(path("/shot"))
            .respond_with(response)
            .mount(server)
            .await;
        Source::Url(format!("{}/shot", server.uri()))
    }

    fn decoded(image: &Image) -> Vec<u8> {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &image.data)
            .expect("base64")
    }

    #[tokio::test]
    async fn a_png_on_disk_is_read_and_passed_through() {
        let dir = tempfile::tempdir().expect("a directory");
        let bytes = drawn(4, 6, ImageFormat::Png);
        let image = load(&wrote(dir.path(), "shot.png", &bytes), None)
            .await
            .expect("a picture");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(decoded(&image), bytes, "the bytes are the file's");
    }

    #[tokio::test]
    async fn a_bmp_on_disk_becomes_a_png() {
        let dir = tempfile::tempdir().expect("a directory");
        let source = wrote(dir.path(), "shot.bmp", &drawn(8, 5, ImageFormat::Bmp));
        let image = load(&source, None).await.expect("a picture");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(png_size(&decoded(&image)), Some((8, 5)));
    }

    /// The extension is not the evidence: a `.png` full of prose is refused
    /// here rather than journaled and refused by a provider.
    #[tokio::test]
    async fn a_file_that_is_not_a_picture_is_refused_whatever_it_is_called() {
        let dir = tempfile::tempdir().expect("a directory");
        let source = wrote(dir.path(), "shot.png", b"not a picture at all");
        assert!(matches!(
            load(&source, None).await,
            Err(PictureError::NotAPicture)
        ));
    }

    #[tokio::test]
    async fn a_path_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().expect("a directory");
        let source = Source::Path(dir.path().join("missing.png"));
        let error = load(&source, None).await.expect_err("no picture");
        assert!(matches!(error, PictureError::Unreadable(_)), "{error}");
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().expect("a directory");
        let source = wrote(dir.path(), "huge.png", &vec![0u8; Image::MAX_BYTES + 1]);
        let error = load(&source, None).await.expect_err("no picture");
        assert!(
            matches!(
                error,
                PictureError::Refused(ImageError::TooLarge { bytes, .. })
                    if bytes == Image::MAX_BYTES + 1
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_jpeg_over_http_is_fetched_by_this_machine_and_passed_through() {
        let server = MockServer::start().await;
        let bytes = drawn(7, 7, ImageFormat::Jpeg);
        let source = serving(
            &server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/jpeg")
                .set_body_bytes(bytes.clone()),
        )
        .await;
        let image = load(&source, None).await.expect("a picture");
        assert_eq!(image.media_type, "image/jpeg");
        assert_eq!(decoded(&image), bytes);
    }

    /// The `Content-Type` is not the evidence either: a server calling a TIFF
    /// a PNG would otherwise journal something no provider can read.
    #[tokio::test]
    async fn a_tiff_over_http_becomes_a_png_whatever_the_header_claims() {
        let server = MockServer::start().await;
        let source = serving(
            &server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(drawn(9, 4, ImageFormat::Tiff)),
        )
        .await;
        let image = load(&source, None).await.expect("a picture");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(png_size(&decoded(&image)), Some((9, 4)));
    }

    /// The gate both the header and the arriving body pass through. It is
    /// asserted here rather than through a served response because only the
    /// header can know the whole size before the bytes come, and which of the
    /// two refused a real response depends on how the socket chunked it —
    /// which is the machine's business, not the test's.
    #[test]
    fn the_cap_refuses_a_size_known_before_the_bytes_are() {
        assert!(refuse_over(0).is_ok());
        assert!(refuse_over(Image::MAX_BYTES as u64).is_ok());
        let error = refuse_over(Image::MAX_BYTES as u64 + 1).expect_err("over the cap");
        assert!(
            matches!(
                error,
                PictureError::Refused(ImageError::TooLarge { bytes, max })
                    if bytes == Image::MAX_BYTES + 1 && max == Image::MAX_BYTES
            ),
            "{error}"
        );
        assert!(refuse_over(u64::MAX).is_err(), "a size no usize holds");
    }

    #[tokio::test]
    async fn a_url_over_the_cap_is_refused() {
        let server = MockServer::start().await;
        let source = serving(
            &server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(vec![0u8; Image::MAX_BYTES + 1]),
        )
        .await;
        let error = load(&source, None).await.expect_err("no picture");
        assert!(
            matches!(error, PictureError::Refused(ImageError::TooLarge { .. })),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_url_that_is_not_there_says_so_and_names_no_url_twice() {
        let server = MockServer::start().await;
        let source = serving(&server, ResponseTemplate::new(404)).await;
        let error = load(&source, None).await.expect_err("no picture");
        assert!(matches!(error, PictureError::Unfetchable(_)), "{error}");
        assert!(error.to_string().contains("404"), "{error}");
        assert!(
            !error.to_string().contains(&server.uri()),
            "the caller names the source: {error}"
        );
    }

    /// A URL a person pasted that is a web page, not a picture.
    #[tokio::test]
    async fn a_page_at_the_end_of_a_url_is_not_a_picture() {
        let server = MockServer::start().await;
        let source = serving(
            &server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/html")
                .set_body_string("<!doctype html><html><body>hello</body></html>"),
        )
        .await;
        assert!(matches!(
            load(&source, None).await,
            Err(PictureError::NotAPicture)
        ));
    }

    // ---- M61: a picture fetched once ------------------------------------

    /// A picture served once, and the count of how many times it was asked
    /// for: the whole of what the cache is about.
    async fn counted(server: &MockServer) -> Source {
        serving(
            server,
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(drawn(6, 6, ImageFormat::Png)),
        )
        .await
    }

    async fn asked(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .map(|all| all.len())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_url_read_twice_is_fetched_once() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = Cache::under(dir.path(), crate::cache::DAYS).expect("a cache");
        let server = MockServer::start().await;
        let source = counted(&server).await;
        let first = load(&source, Some(&cache)).await.expect("a picture");
        let again = load(&source, Some(&cache)).await.expect("a picture");
        assert_eq!(asked(&server).await, 1, "the second reading is the cache's");
        assert_eq!(decoded(&first), decoded(&again), "and the very same bytes");
    }

    /// A path is the file itself, so nothing is copied under the data
    /// directory for it (non-goal, M61).
    #[tokio::test]
    async fn a_file_on_this_machine_is_never_copied_into_the_cache() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = Cache::under(dir.path(), crate::cache::DAYS).expect("a cache");
        let source = wrote(dir.path(), "shot.png", &drawn(3, 3, ImageFormat::Png));
        load(&source, Some(&cache)).await.expect("a picture");
        assert!(!dir.path().join(crate::cache::DIR).exists());
    }

    /// An entry past its time is fetched again and written again, so the
    /// picture a person sees is never a fortnight stale.
    #[tokio::test]
    async fn an_entry_past_its_time_is_fetched_again_and_rewritten() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = Cache::under(dir.path(), 1).expect("a cache");
        let server = MockServer::start().await;
        let source = counted(&server).await;
        let Source::Url(url) = &source else {
            panic!("a url");
        };
        load(&source, Some(&cache)).await.expect("a picture");
        let entry = cache.path(url);
        let stale = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
        std::fs::File::options()
            .write(true)
            .open(&entry)
            .expect("the entry")
            .set_modified(stale)
            .expect("a stamp this machine takes");
        load(&source, Some(&cache)).await.expect("a picture");
        assert_eq!(asked(&server).await, 2, "the stale entry was passed over");
        let mtime = std::fs::metadata(&entry)
            .and_then(|at| at.modified())
            .expect("a stamp");
        assert!(mtime > stale, "and the entry was written again");
    }

    /// `0` days is no cache: every reading is a fetch and nothing is written.
    #[tokio::test]
    async fn no_days_writes_nothing_and_fetches_every_time() {
        let dir = tempfile::tempdir().expect("a directory");
        assert!(Cache::under(dir.path(), 0).is_none(), "no cache to pass");
        let server = MockServer::start().await;
        let source = counted(&server).await;
        load(&source, None).await.expect("a picture");
        load(&source, None).await.expect("a picture");
        assert_eq!(asked(&server).await, 2);
        assert!(!dir.path().join(crate::cache::DIR).exists());
    }

    /// A server that is not there is not a picture, and the miss is not kept:
    /// nothing may cache a failure as though it were bytes.
    #[tokio::test]
    async fn a_fetch_that_failed_leaves_no_entry_behind() {
        let dir = tempfile::tempdir().expect("a directory");
        let cache = Cache::under(dir.path(), crate::cache::DAYS).expect("a cache");
        let server = MockServer::start().await;
        let source = serving(&server, ResponseTemplate::new(404)).await;
        let Source::Url(url) = &source else {
            panic!("a url");
        };
        load(&source, Some(&cache)).await.expect_err("no picture");
        assert!(!cache.path(url).exists());
    }
}
