//! A job's log: `<data_dir>/bash/<id>.log`.
//!
//! The file is the one representation of what a background command wrote
//! (ADR-0018 §3). The reader appends to it as the process writes, `BashOutput`
//! is a window over it at a byte cursor, and a person may `tail -f` it while
//! it fills. It is capped by size, and where the cap bites the log says so in
//! its own text, so someone reading the file alone is never misled about what
//! is missing.

use std::io;
use std::path::{Path, PathBuf};

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

/// Bytes one job's log may hold. A long build fits; a runaway `yes` does not
/// fill the disk.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// The directory every job's log lives in.
pub fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join("bash")
}

/// The line the log carries where the cap bit. It names the cap, because the
/// file is read by people and tools that never saw this constant.
fn capped() -> String {
    format!(
        "\n[… this log is capped at {} MiB; nothing after this point was kept …]\n",
        MAX_BYTES / (1024 * 1024)
    )
}

/// One job's log, open for appending.
#[derive(Debug)]
pub struct Log {
    path: PathBuf,
    file: File,
    written: u64,
    full: bool,
}

impl Log {
    /// A fresh log for `id` under `dir`, its directory made if it is not there.
    pub async fn create(dir: &Path, id: &str) -> io::Result<Self> {
        fs::create_dir_all(dir).await?;
        let path = dir.join(format!("{id}.log"));
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            file,
            written: 0,
            full: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append what the command wrote, up to the cap. The first write past the
    /// cap leaves the note and nothing after it.
    pub async fn write(&mut self, text: &str) -> io::Result<()> {
        if self.full {
            return Ok(());
        }
        let room = MAX_BYTES.saturating_sub(self.written) as usize;
        if text.len() <= room {
            return self.append(text.as_bytes()).await;
        }
        let kept = &text[..floor_char_boundary(text, room)];
        self.append(kept.as_bytes()).await?;
        self.full = true;
        self.file.write_all(capped().as_bytes()).await?;
        self.file.flush().await
    }

    /// A line the plugin itself has to leave behind — a notification that
    /// reached nobody. It is written whether or not the cap has bitten: it is
    /// short, and it is the only trace of what went wrong.
    pub async fn note(&mut self, line: &str) -> io::Result<()> {
        self.file
            .write_all(format!("\n[{line}]\n").as_bytes())
            .await?;
        self.file.flush().await
    }

    async fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes).await?;
        self.written += bytes.len() as u64;
        self.file.flush().await
    }
}

/// The greatest index up to `at` that starts a character.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// What a log holds from one cursor on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub text: String,
    /// Where to read on from; a cursor never lands inside a character.
    pub cursor: u64,
    /// There is more in the log past `cursor`.
    pub more: bool,
}

/// Read at most `max` bytes from `cursor` on. Bytes that do not yet form a
/// whole character are left for the next call, so a window never cuts one in
/// two and a cursor is always safe to pass back.
pub async fn window(path: &Path, cursor: u64, max: usize) -> io::Result<Window> {
    let mut file = File::open(path).await?;
    file.seek(io::SeekFrom::Start(cursor)).await?;
    let mut bytes = vec![0u8; max];
    let mut read = 0;
    while read < max {
        match file.read(&mut bytes[read..]).await? {
            0 => break,
            n => read += n,
        }
    }
    let (text, used) = decode(&bytes[..read]);
    let cursor = cursor + used as u64;
    Ok(Window {
        text,
        cursor,
        more: file.metadata().await?.len() > cursor,
    })
}

/// Bytes as text, and how many of them were consumed. A chunk that ends
/// mid-character keeps the tail for the next read; a byte that can never
/// start one is one replacement, so a cursor can never stick on it.
fn decode(bytes: &[u8]) -> (String, usize) {
    let error = match std::str::from_utf8(bytes) {
        Ok(text) => return (text.to_string(), bytes.len()),
        Err(error) => error,
    };
    let valid = error.valid_up_to();
    let mut text = String::from_utf8_lossy(&bytes[..valid]).into_owned();
    match error.error_len() {
        Some(broken) => {
            text.push(char::REPLACEMENT_CHARACTER);
            (text, valid + broken)
        }
        None => (text, valid),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn written(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "ab12cd34").await.expect("a log");
        log.write(text).await.expect("written");
        let path = log.path().to_path_buf();
        (dir, path)
    }

    #[tokio::test]
    async fn a_log_is_the_id_under_the_bash_directory() {
        let data = tempfile::tempdir().expect("temp dir");
        let log = Log::create(&dir(data.path()), "ab12cd34")
            .await
            .expect("a log");
        assert!(
            log.path().ends_with("bash/ab12cd34.log"),
            "{:?}",
            log.path()
        );
        assert!(log.path().exists());
    }

    #[tokio::test]
    async fn what_was_written_is_what_the_window_reads_back() {
        let (_dir, path) = written("one\ntwo\n").await;
        let read = window(&path, 0, 1_000).await.expect("a window");
        assert_eq!(read.text, "one\ntwo\n");
        assert_eq!(read.cursor, 8);
        assert!(!read.more);
    }

    #[tokio::test]
    async fn a_cursor_reads_on_from_where_the_last_one_stopped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "job").await.expect("a log");
        log.write("first\n").await.expect("written");
        let path = log.path().to_path_buf();

        let one = window(&path, 0, 1_000).await.expect("a window");
        assert_eq!(one.text, "first\n");
        log.write("second\n").await.expect("written");
        let two = window(&path, one.cursor, 1_000).await.expect("a window");
        assert_eq!(two.text, "second\n");
        let none = window(&path, two.cursor, 1_000).await.expect("a window");
        assert_eq!(none.text, "");
        assert_eq!(none.cursor, two.cursor, "an empty read leaves the cursor");
    }

    #[tokio::test]
    async fn a_window_says_when_there_is_more_waiting() {
        let (_dir, path) = written("0123456789").await;
        let read = window(&path, 0, 4).await.expect("a window");
        assert_eq!(read.text, "0123");
        assert!(read.more);
        assert!(!window(&path, 0, 10).await.expect("a window").more);
        assert!(!window(&path, 4, 6).await.expect("a window").more);
    }

    #[tokio::test]
    async fn a_character_split_across_two_windows_waits_for_the_second() {
        let (_dir, path) = written("字a").await;
        // Two of the three bytes of 字: nothing whole, so nothing is read.
        let cut = window(&path, 0, 2).await.expect("a window");
        assert_eq!(cut.text, "");
        assert_eq!(cut.cursor, 0);
        let whole = window(&path, 0, 3).await.expect("a window");
        assert_eq!(whole.text, "字");
        assert_eq!(whole.cursor, 3);
        assert!(whole.more, "the `a` is still waiting");
    }

    #[tokio::test]
    async fn the_cap_stops_the_log_and_says_so_in_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "job").await.expect("a log");
        let block = "y".repeat(1024 * 1024);
        for _ in 0..6 {
            log.write(&block).await.expect("written");
        }
        let path = log.path().to_path_buf();
        let read = window(&path, 0, 8 * 1024 * 1024).await.expect("a window");
        assert!(read.text.contains("capped at 4 MiB"), "no cap marker");
        assert_eq!(
            read.text.chars().filter(|c| *c == 'y').count() as u64,
            MAX_BYTES,
            "the cap counts what was kept"
        );
    }

    #[tokio::test]
    async fn a_note_is_written_even_once_the_cap_has_bitten() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "job").await.expect("a log");
        log.write(&"y".repeat(MAX_BYTES as usize + 10))
            .await
            .expect("written");
        log.note("nobody was there to be told")
            .await
            .expect("noted");
        let path = log.path().to_path_buf();
        let read = window(&path, 0, 8 * 1024 * 1024).await.expect("a window");
        assert!(
            read.text.ends_with("[nobody was there to be told]\n"),
            "no note"
        );
    }

    #[test]
    fn a_broken_byte_is_one_replacement_and_the_cursor_moves_past_it() {
        let (text, used) = decode(b"a\xffb");
        assert_eq!(text, "a\u{fffd}");
        assert_eq!(used, 2, "the read stops after the bad byte");
    }
}
