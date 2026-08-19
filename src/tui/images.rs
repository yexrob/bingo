//! The content-image registry (D97): what the session has *shown*, and how to
//! open one in the desktop's viewer.
//!
//! A terminal can place an image but it cannot zoom one. Four cells of a
//! screenshot is enough to know a screenshot arrived and never enough to read
//! it, so the picture on screen has to have a door out of the terminal — and
//! the thing behind the door is an ordinary file the system viewer already
//! knows how to open.
//!
//! **Content, not chrome.** Avatars are drawn with the same kitty machinery and
//! are deliberately absent here: a portrait is furniture the interface wears,
//! not something the conversation said. The rule is the one the module name
//! states — an entry exists because a picture entered the conversation.
//!
//! **The session's own data, not the request's.** Registration happens where an
//! image enters *rendering*, which is upstream of the D93 vision projection.
//! That projection is a view of the history taken at the send seam; the history
//! keeps its image blocks either way, so switching to a model without vision
//! empties the payload and changes nothing here.
//!
//! **Bounded, and it only deletes its own.** Images the session read off disk
//! are addressed where they already live and are never copied or removed.
//! Images that exist only in memory — a clipboard paste, a fetched URL — are
//! written into a pid-tagged temp directory the first time somebody asks to
//! open one, and evicted oldest-first past [`MAX_ENTRIES`] or [`MAX_BYTES`].
//! Eviction removes files by the exact path it wrote and never a directory.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Newest-first entries the registry keeps. Past this the oldest is evicted:
/// the point of the list is "what did I just see", and a hundred is already
/// more than anybody scrolls.
pub const MAX_ENTRIES: usize = 100;

/// Bytes the registry may hold in memory. A screenshot is a megabyte or two,
/// so fifty is a long session's worth and still far below anything that would
/// be felt.
pub const MAX_BYTES: usize = 50 * 1024 * 1024;

/// What is behind an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The image is a file that already exists and belongs to somebody else —
    /// a path the user attached, a file a tool read. Opened in place; never
    /// copied, never deleted.
    OnDisk(PathBuf),
    /// The image exists only in memory (a clipboard paste, a fetched URL).
    /// Written to a temp file on first open, and that file is ours to remove.
    Memory(Vec<u8>),
}

/// One content image the session has shown.
#[derive(Debug, Clone)]
pub struct ImageEntry {
    /// 1-based, in registration order. Stable for the life of the session.
    pub id: usize,
    /// Where it came from, in the words the user would use: `clipboard`, a
    /// path, a URL.
    pub source: String,
    /// Unix seconds, 0 when the source carried no clock.
    pub at: u64,
    pub bytes: usize,
    /// `png` / `jpeg` / `gif` / `webp`, or `image` when the magic says nothing.
    pub format: &'static str,
    /// The `#[image N]` marker this image answers to, where it has one.
    pub marker: Option<usize>,
    origin: Origin,
    /// The temp file written for a [`Origin::Memory`] entry, once written.
    file: Option<PathBuf>,
}

impl ImageEntry {
    /// The picker's line: `clipboard · 14:02 · 218 kB`.
    pub fn label(&self) -> String {
        let when = crate::tui::buffer::stamp(self.at);
        let size = size_label(self.bytes);
        if when.is_empty() {
            format!("{} · {size}", self.source)
        } else {
            format!("{} · {when} · {size}", self.source)
        }
    }
}

/// `218 kB` / `1.4 MB` — a size somebody can judge at a glance.
pub fn size_label(bytes: usize) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
        b if b >= 1024 => format!("{} kB", b / 1024),
        b => format!("{b} B"),
    }
}

/// Why an open did not happen. Every arm reads as one line under the composer.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("no such image")]
    Unknown,
    #[error("{0}")]
    Write(String),
    #[error("{0}")]
    Spawn(String),
}

/// The session's content images, newest last in storage and newest first to
/// every reader.
#[derive(Debug)]
pub struct ImageRegistry {
    entries: Vec<ImageEntry>,
    next_id: usize,
    held: usize,
    dir: PathBuf,
}

impl Default for ImageRegistry {
    fn default() -> Self {
        Self::new(default_dir())
    }
}

/// The temp directory this process materializes into. Pid-tagged, so two
/// bingos on one machine cannot write over each other (the mistake
/// `gfx::clipboard_image_png`'s fixed `/tmp` path still makes).
pub fn default_dir() -> PathBuf {
    std::env::temp_dir().join(format!("bingo-images-{}", std::process::id()))
}

impl ImageRegistry {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            entries: Vec::new(),
            next_id: 1,
            held: 0,
            dir,
        }
    }

    /// Newest first — the order every reader wants and nobody should re-derive.
    pub fn newest_first(&self) -> Vec<&ImageEntry> {
        self.entries.iter().rev().collect()
    }

    /// One entry by id.
    pub fn get(&self, id: usize) -> Option<&ImageEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The entry a `#[image N]` marker addresses.
    pub fn by_marker(&self, marker: usize) -> Option<&ImageEntry> {
        self.entries.iter().find(|e| e.marker == Some(marker))
    }

    /// The entry a rendered image row addresses, by the URL its [`ImageRef`]
    /// carries.
    ///
    /// [`ImageRef`]: crate::tui::line::ImageRef
    pub fn by_source(&self, source: &str) -> Option<&ImageEntry> {
        self.entries.iter().find(|e| e.source == source)
    }

    /// Register an image held in memory (clipboard, fetched URL).
    pub fn register_bytes(&mut self, source: &str, at: u64, bytes: Vec<u8>) -> usize {
        let len = bytes.len();
        let format = sniff(&bytes);
        self.push(source, at, len, format, Origin::Memory(bytes))
    }

    /// Register an image that is already a file. Nothing is copied and nothing
    /// is read: the path *is* the image, which is why a tool that read a
    /// screenshot costs the registry a struct and no bytes.
    pub fn register_file(&mut self, path: &Path, at: u64, bytes: usize) -> usize {
        let format = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .and_then(|e| match e.as_str() {
                "png" => Some("png"),
                "jpg" | "jpeg" => Some("jpeg"),
                "gif" => Some("gif"),
                "webp" => Some("webp"),
                _ => None,
            })
            .unwrap_or("image");
        let source = path.display().to_string();
        self.push(
            &source,
            at,
            bytes,
            format,
            Origin::OnDisk(path.to_path_buf()),
        )
    }

    /// Tie a registry entry to the `#[image N]` marker the composer inserted,
    /// so a click on the marker row finds the picture behind it.
    pub fn set_marker(&mut self, id: usize, marker: usize) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.marker = Some(marker);
        }
    }

    fn push(
        &mut self,
        source: &str,
        at: u64,
        bytes: usize,
        format: &'static str,
        origin: Origin,
    ) -> usize {
        // The same picture arriving twice — a re-render, a URL loaded again —
        // is one entry. Identity is the content plus where it came from: two
        // different files that happen to be byte-identical are still two
        // things the user can point at.
        let key = fingerprint(source, &origin, bytes);
        if let Some(existing) = self
            .entries
            .iter()
            .find(|e| fingerprint(&e.source, &e.origin, e.bytes) == key)
        {
            return existing.id;
        }
        let id = self.next_id;
        self.next_id += 1;
        if let Origin::Memory(data) = &origin {
            self.held += data.len();
        }
        self.entries.push(ImageEntry {
            id,
            source: source.to_string(),
            at,
            bytes,
            format,
            marker: None,
            origin,
            file: None,
        });
        self.evict();
        id
    }

    /// Oldest-first eviction down to both bounds. The only thing removed from
    /// the filesystem is a temp file this registry wrote, addressed by the
    /// exact path it recorded.
    fn evict(&mut self) {
        while self.entries.len() > MAX_ENTRIES || (self.held > MAX_BYTES && self.entries.len() > 1)
        {
            let entry = self.entries.remove(0);
            if let Origin::Memory(data) = &entry.origin {
                self.held = self.held.saturating_sub(data.len());
            }
            if let Some(path) = &entry.file {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// The file behind an entry, written now if it was only ever in memory.
    pub fn materialize(&mut self, id: usize) -> Result<PathBuf, OpenError> {
        let dir = self.dir.clone();
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.id == id)
            .ok_or(OpenError::Unknown)?;
        match &entry.origin {
            Origin::OnDisk(path) => Ok(path.clone()),
            Origin::Memory(data) => {
                if let Some(path) = &entry.file
                    && path.exists()
                {
                    return Ok(path.clone());
                }
                std::fs::create_dir_all(&dir).map_err(|e| OpenError::Write(e.to_string()))?;
                let path = dir.join(format!("image-{id}.{}", entry.format));
                std::fs::write(&path, data).map_err(|e| OpenError::Write(e.to_string()))?;
                entry.file = Some(path.clone());
                Ok(path)
            }
        }
    }
}

/// Identity of an image for deduplication: cheap, and stable across renders.
fn fingerprint(source: &str, origin: &Origin, bytes: usize) -> u64 {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    bytes.hash(&mut hasher);
    if let Origin::Memory(data) = origin {
        // The head is enough to separate two pictures of the same size; the
        // whole buffer would be hashed on every paste for no extra certainty.
        data[..data.len().min(4096)].hash(&mut hasher);
    }
    hasher.finish()
}

/// Format from the magic bytes, because a clipboard paste has no filename.
fn sniff(bytes: &[u8]) -> &'static str {
    match bytes {
        b if b.starts_with(b"\x89PNG\r\n\x1a\n") => "png",
        [0xFF, 0xD8, 0xFF, ..] => "jpeg",
        b if b.starts_with(b"GIF8") => "gif",
        b if b.len() > 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" => "webp",
        _ => "image",
    }
}

/// The desktop's opener for this platform: the program, and the arguments that
/// come before the path.
///
/// Same three-way split `share::open_in_browser` makes — `open` hands a file to
/// whatever the user set as its handler, and so do the other two — with the
/// program left as a value so a test can point it somewhere harmless.
pub fn desktop_opener() -> (&'static str, &'static [&'static str]) {
    if cfg!(target_os = "macos") {
        ("open", &[])
    } else if cfg!(target_os = "windows") {
        // `start` reads its first quoted argument as the window title, so the
        // empty string is what keeps a path with spaces from becoming one.
        ("cmd", &["/c", "start", ""])
    } else {
        ("xdg-open", &[])
    }
}

/// Hand `path` to `program` and walk away.
///
/// `spawn`, never `status`: the viewer outlives the keystroke that opened it,
/// and waiting on it would freeze the TUI behind somebody else's window. The
/// path goes in as one argument — there is no shell here to interpolate it.
pub fn open_detached(program: &str, leading: &[&str], path: &Path) -> Result<(), OpenError> {
    let mut command = std::process::Command::new(program);
    command.args(leading).arg(path);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| OpenError::Spawn(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(n: u8) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend(std::iter::repeat_n(n, 64));
        bytes
    }

    fn registry() -> ImageRegistry {
        ImageRegistry::new(std::env::temp_dir().join(format!(
            "bingo-d97-{}-{:?}",
            std::process::id(),
            "reg"
        )))
    }

    /// The list is a memory: the last thing seen is the first thing offered.
    #[test]
    fn entries_come_back_newest_first_with_their_labels() {
        let mut reg = registry();
        let a = reg.register_bytes("clipboard", 0, png(1));
        let b = reg.register_bytes("https://example.com/plot.png", 0, png(2));
        let order: Vec<usize> = reg.newest_first().iter().map(|e| e.id).collect();
        assert_eq!(order, vec![b, a], "newest first");
        let newest = reg.newest_first()[0];
        assert!(
            newest.label().contains("plot.png") && newest.label().contains('B'),
            "the label names the source and its size: {}",
            newest.label()
        );
        assert_eq!(newest.format, "png", "the magic bytes name the format");
    }

    /// A picture that renders twice is one picture. Without this the registry
    /// grows a copy per repaint of the same URL.
    #[test]
    fn the_same_image_registers_once() {
        let mut reg = registry();
        let first = reg.register_bytes("clipboard", 0, png(1));
        assert_eq!(
            reg.register_bytes("clipboard", 0, png(1)),
            first,
            "same source and same bytes is the same entry"
        );
        assert_eq!(reg.newest_first().len(), 1);
        assert_ne!(
            reg.register_bytes("clipboard", 0, png(2)),
            first,
            "different bytes is a different picture"
        );
    }

    /// Eviction is bounded and it deletes only what it wrote.
    #[test]
    fn eviction_drops_the_oldest_and_removes_only_its_own_file() {
        let dir = std::env::temp_dir().join(format!("bingo-d97-evict-{}", std::process::id()));
        let mut reg = ImageRegistry::new(dir.clone());
        // A file that belongs to somebody else: registered by path, so nothing
        // here may ever remove it.
        let theirs = dir.join("theirs.png");
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(&theirs, png(9)).expect("write");
        let kept = reg.register_file(&theirs, 0, 72);

        let first = reg.register_bytes("clipboard", 0, png(1));
        let path = reg.materialize(first).expect("materialize");
        assert!(path.exists(), "materializing writes the file");

        for n in 2..=(MAX_ENTRIES as u8 + 2) {
            reg.register_bytes("clipboard", 0, png(n));
        }
        assert!(
            reg.newest_first().len() <= MAX_ENTRIES,
            "the bound holds: {}",
            reg.newest_first().len()
        );
        assert!(reg.get(first).is_none(), "the oldest went");
        assert!(!path.exists(), "and its temp file went with it");
        assert!(
            reg.get(kept).is_none() && theirs.exists(),
            "a path we did not write is dropped from the list and left on disk"
        );
        std::fs::remove_file(&theirs).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file the session read is opened where it lives — no copy, no second
    /// truth about which bytes the user is looking at.
    #[test]
    fn an_on_disk_image_materializes_to_itself() {
        let dir = std::env::temp_dir().join(format!("bingo-d97-disk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("shot.png");
        std::fs::write(&file, png(3)).expect("write");
        let mut reg = ImageRegistry::new(dir.clone());
        let id = reg.register_file(&file, 1_700_000_000, 72);
        assert_eq!(reg.materialize(id).expect("materialize"), file);
        assert_eq!(reg.get(id).map(|e| e.format), Some("png"));
        std::fs::remove_file(&file).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The opener is a value, so the acceptance path can be driven without a
    /// desktop — and the platform arm is the one `share.rs` already settled on.
    #[test]
    fn the_opener_is_the_platforms_and_the_spawn_is_detached() {
        let (program, leading) = desktop_opener();
        if cfg!(target_os = "macos") {
            assert_eq!((program, leading.len()), ("open", 0));
        } else if cfg!(target_os = "windows") {
            assert_eq!(program, "cmd");
            assert_eq!(leading, ["/c", "start", ""]);
        } else {
            assert_eq!((program, leading.len()), ("xdg-open", 0));
        }
        let missing = open_detached(
            "bingo-no-such-viewer-d97",
            &[],
            Path::new("/nonexistent.png"),
        );
        assert!(
            matches!(missing, Err(OpenError::Spawn(_))),
            "a missing viewer is an error, not a panic"
        );
    }

    #[test]
    fn sizes_read_at_a_glance() {
        assert_eq!(size_label(512), "512 B");
        assert_eq!(size_label(4096), "4 kB");
        assert_eq!(size_label(3 * 1024 * 1024), "3.0 MB");
    }
}
