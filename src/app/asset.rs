//! Bytes the server owns.
//!
//! Registration takes a local path, checks it against whatever the caller
//! claimed about it, and **copies the bytes into the server's own storage**. The
//! caller's path is never borrowed afterwards and never deleted: a client that
//! wrote a temporary file for a clipboard image may remove it the moment
//! registration succeeds, and an asset a transcript refers to cannot be changed
//! under the session by editing the file it came from (spec "Client request
//! taxonomy").
//!
//! Reading is chunked and bounded, so one frame never has to carry a whole
//! image, and the same path serves the other producer of large bytes: output too
//! big for an item keeps a bounded preview in the item and becomes an artifact
//! read exactly like an image.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha256};

use crate::app::ids::{AssetId, IdMint, UnixMillis, now_millis};
use crate::app::snapshot::{AssetKind, AssetOrigin, AssetRecord, ImageInfo};

/// The largest file registration will take. Beyond this the caller is asking the
/// server to hold a working set, not an attachment.
pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

/// The most bytes one `asset/readChunk` returns, before base64 expands them by a
/// third — comfortably under the server frame ceiling with the envelope's own
/// room to spare.
pub const MAX_CHUNK_BYTES: u32 = 1024 * 1024;

/// Why an asset was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssetError {
    #[error("no such asset")]
    NotFound,
    #[error("{0}")]
    Rejected(String),
    #[error("bad argument: {0}")]
    BadArgument(String),
}

/// One asset the server holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stored {
    record: AssetRecord,
    path: PathBuf,
    /// A short label a listing shows — the file it came from, or what produced it.
    label: Option<String>,
}

/// The assets of one session.
///
/// Its directory is the epoch's, so two runs of the process cannot collide and
/// closing the session takes its assets with it.
#[derive(Debug, Default)]
pub struct AssetStore {
    dir: PathBuf,
    assets: BTreeMap<AssetId, Stored>,
}

impl AssetStore {
    /// Where the bytes go. An empty root means the session has nowhere to put
    /// them, and registration says so rather than writing somewhere arbitrary.
    pub fn new(root: &Path, epoch: &str) -> Self {
        Self {
            dir: if root.as_os_str().is_empty() {
                PathBuf::new()
            } else {
                root.join("assets").join(epoch)
            },
            assets: BTreeMap::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Take a local file into server storage.
    pub fn register_path(
        &mut self,
        mint: &mut IdMint,
        path: &Path,
        expected_mime: Option<&str>,
        expected_sha256: Option<&str>,
    ) -> Result<AssetRecord, AssetError> {
        let meta = std::fs::metadata(path)
            .map_err(|error| AssetError::Rejected(format!("cannot read the file: {error}")))?;
        if !meta.is_file() {
            return Err(AssetError::Rejected("not a file".to_string()));
        }
        if meta.len() > MAX_ASSET_BYTES {
            return Err(AssetError::Rejected(format!(
                "the file is larger than the {MAX_ASSET_BYTES} byte limit"
            )));
        }
        let bytes = std::fs::read(path)
            .map_err(|error| AssetError::Rejected(format!("cannot read the file: {error}")))?;
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string());
        self.register_bytes(mint, &bytes, expected_mime, expected_sha256, label)
    }

    /// Take bytes the server itself produced — a tool's oversized output, a
    /// command's final text — into the same storage, read back the same way.
    pub fn register_bytes(
        &mut self,
        mint: &mut IdMint,
        bytes: &[u8],
        expected_mime: Option<&str>,
        expected_sha256: Option<&str>,
        label: Option<String>,
    ) -> Result<AssetRecord, AssetError> {
        let sha256 = digest(bytes);
        if let Some(expected) = expected_sha256
            && !expected.eq_ignore_ascii_case(&sha256)
        {
            return Err(AssetError::Rejected(
                "the file does not match the expected SHA-256".to_string(),
            ));
        }
        let (kind, mime, dimensions) = classify(bytes);
        if let Some(expected) = expected_mime
            && expected != mime
        {
            return Err(AssetError::Rejected(format!(
                "the file is {mime}, not the expected {expected}"
            )));
        }
        if self.dir.as_os_str().is_empty() {
            return Err(AssetError::Rejected(
                "this session has nowhere to store an asset".to_string(),
            ));
        }
        let id: AssetId = mint.mint();
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| AssetError::Rejected(format!("cannot open the store: {error}")))?;
        let path = self.dir.join(id.as_str());
        std::fs::write(&path, bytes)
            .map_err(|error| AssetError::Rejected(format!("cannot write the asset: {error}")))?;
        let record = AssetRecord {
            id: id.clone(),
            kind,
            origin: AssetOrigin::Session,
            mime: mime.to_string(),
            bytes: bytes.len() as u64,
            sha256,
            width: dimensions.map(|(width, _)| width),
            height: dimensions.map(|(_, height)| height),
            created_at: now_millis(),
        };
        self.assets.insert(
            id,
            Stored {
                record: record.clone(),
                path,
                label,
            },
        );
        Ok(record)
    }

    /// One bounded chunk, base64, with where to continue and whether that was
    /// the end.
    pub fn read_chunk(
        &self,
        id: &AssetId,
        offset: u64,
        length: u32,
    ) -> Result<(String, u64, bool), AssetError> {
        if length == 0 {
            return Err(AssetError::BadArgument("a chunk of no bytes".to_string()));
        }
        let stored = self.assets.get(id).ok_or(AssetError::NotFound)?;
        let total = stored.record.bytes;
        if offset > total {
            return Err(AssetError::BadArgument(
                "the offset is past the end of the asset".to_string(),
            ));
        }
        let length = u64::from(length.min(MAX_CHUNK_BYTES));
        let end = offset.saturating_add(length).min(total);
        let bytes = std::fs::read(&stored.path).map_err(|_| AssetError::NotFound)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let stop = usize::try_from(end).unwrap_or(usize::MAX).min(bytes.len());
        Ok((BASE64.encode(&bytes[start..stop]), end, end >= total))
    }

    pub fn record(&self, id: &AssetId) -> Option<&AssetRecord> {
        self.assets.get(id).map(|stored| &stored.record)
    }

    /// The images this session holds, newest first — the `images` catalog.
    pub fn images(&self) -> Vec<ImageInfo> {
        let mut images: Vec<(UnixMillis, ImageInfo)> = self
            .assets
            .values()
            .filter(|stored| stored.record.kind == AssetKind::Image)
            .map(|stored| {
                (
                    stored.record.created_at,
                    ImageInfo {
                        asset_id: stored.record.id.clone(),
                        label: stored.label.clone(),
                        width: stored.record.width.unwrap_or(0),
                        height: stored.record.height.unwrap_or(0),
                        bytes: stored.record.bytes,
                        created_at: stored.record.created_at,
                    },
                )
            })
            .collect();
        images.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        images.into_iter().map(|(_, info)| info).collect()
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// Drop everything this session owns. Session assets die with the session;
    /// what a transcript refers to stays reconstructable from its own durable
    /// content.
    pub fn clear(&mut self) {
        self.assets.clear();
        if !self.dir.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// What the bytes are, read from the bytes rather than from the file's name: an
/// extension is a claim, and a claim is what the expected-MIME check is for.
fn classify(bytes: &[u8]) -> (AssetKind, &'static str, Option<(u32, u32)>) {
    if let Ok(reader) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()
        && let Some(format) = reader.format()
    {
        let mime = match format {
            image::ImageFormat::Png => "image/png",
            image::ImageFormat::Jpeg => "image/jpeg",
            image::ImageFormat::Gif => "image/gif",
            image::ImageFormat::WebP => "image/webp",
            _ => "application/octet-stream",
        };
        let dimensions = reader.into_dimensions().ok();
        return (AssetKind::Image, mime, dimensions);
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => (AssetKind::Text, "text/plain", None),
        Err(_) => (AssetKind::Binary, "application/octet-stream", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;

    fn store(tag: &str) -> (AssetStore, IdMint, PathBuf) {
        let root = std::env::temp_dir().join(format!("bingo-asset-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let epoch = EpochId::mint();
        let store = AssetStore::new(&root, epoch.as_str());
        (store, IdMint::new(epoch), root)
    }

    /// A one-pixel PNG, so the classifier has a real image to read.
    fn png() -> Vec<u8> {
        let mut out = Vec::new();
        let image = image::RgbaImage::from_pixel(2, 3, image::Rgba([1, 2, 3, 255]));
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        out
    }

    /// Register, then read the bytes back through the chunked path and check
    /// they are the same bytes with the same digest.
    #[test]
    fn an_asset_comes_back_byte_for_byte() {
        let (mut store, mut mint, root) = store("roundtrip");
        let source = root.join("source.png");
        let bytes = png();
        let _ = std::fs::create_dir_all(&root);
        std::fs::write(&source, &bytes).unwrap_or_else(|error| panic!("{error}"));

        let record = store
            .register_path(&mut mint, &source, Some("image/png"), None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(record.kind, AssetKind::Image);
        assert_eq!(record.bytes, bytes.len() as u64);
        assert_eq!(record.width, Some(2));
        assert_eq!(record.height, Some(3));
        assert_eq!(record.sha256, digest(&bytes));

        // The caller's file may go the moment registration succeeded.
        std::fs::remove_file(&source).unwrap_or_else(|error| panic!("{error}"));

        let mut read = Vec::new();
        let mut offset = 0;
        loop {
            let (data, next, eof) = store
                .read_chunk(&record.id, offset, 16)
                .unwrap_or_else(|error| panic!("{error}"));
            read.extend(
                BASE64
                    .decode(data)
                    .unwrap_or_else(|error| panic!("{error}")),
            );
            offset = next;
            if eof {
                break;
            }
        }
        assert_eq!(read, bytes, "the bytes survive the round trip");
        assert_eq!(digest(&read), record.sha256);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What the caller claimed is checked against what the bytes are.
    #[test]
    fn a_claim_that_does_not_hold_is_refused() {
        let (mut store, mut mint, root) = store("claims");
        let _ = std::fs::create_dir_all(&root);
        let source = root.join("note.txt");
        std::fs::write(&source, b"hello").unwrap_or_else(|error| panic!("{error}"));

        assert!(matches!(
            store.register_path(&mut mint, &source, Some("image/png"), None),
            Err(AssetError::Rejected(_))
        ));
        assert!(matches!(
            store.register_path(&mut mint, &source, None, Some("0".repeat(64).as_str())),
            Err(AssetError::Rejected(_))
        ));
        assert!(
            store.is_empty(),
            "a refused registration stores nothing at all"
        );
        let record = store
            .register_path(&mut mint, &source, Some("text/plain"), None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(record.kind, AssetKind::Text);
        assert!(matches!(
            store.read_chunk(&AssetId::new("asset_nope"), 0, 8),
            Err(AssetError::NotFound)
        ));
        assert!(matches!(
            store.read_chunk(&record.id, 999, 8),
            Err(AssetError::BadArgument(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The session's own assets go when the session does.
    #[test]
    fn closing_the_session_takes_its_assets_with_it() {
        let (mut store, mut mint, root) = store("cleanup");
        let record = store
            .register_bytes(&mut mint, &png(), None, None, Some("pasted".to_string()))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            store
                .images()
                .into_iter()
                .map(|image| image.asset_id)
                .collect::<Vec<_>>(),
            vec![record.id.clone()],
            "an image is in the images catalog"
        );
        let dir = store.dir().to_path_buf();
        assert!(dir.exists());
        store.clear();
        assert!(store.is_empty());
        assert!(!dir.exists(), "the bytes went with the session");
        let _ = std::fs::remove_dir_all(&root);
    }
}
