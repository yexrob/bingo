use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::api::types::Message;
use crate::error::ErrorCode;

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("transcript io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse transcript line: {0}")]
    Parse(#[from] serde_json::Error),
}

impl ErrorCode for TranscriptError {
    fn error_code(&self) -> &'static str {
        match self {
            TranscriptError::Io(_) | TranscriptError::Parse(_) => "STORAGE_ERROR",
        }
    }
}

/// Session transcript: JSONL, one Message per line (D11).
#[derive(Debug, Clone)]
pub struct Transcript {
    path: PathBuf,
}

fn slugify(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "root".to_string()
    } else {
        cleaned
    }
}

/// transcripts dir: ~/.local/share/bingo/transcripts.
pub fn transcripts_dir(home: &Path) -> PathBuf {
    home.join(".local")
        .join("share")
        .join("bingo")
        .join("transcripts")
}

/// New session file: <project-slug>-<unix-ts>.jsonl.
pub fn create(home: &Path, cwd: &Path) -> Result<Transcript, TranscriptError> {
    let dir = transcripts_dir(home);
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = slugify(&name);
    let path = dir.join(format!("{slug}-{ts}.jsonl"));
    Ok(Transcript { path })
}

/// All sessions (/resume list), most recently modified first.
pub fn list(home: &Path) -> Result<Vec<Transcript>, TranscriptError> {
    let dir = transcripts_dir(home);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(SystemTime, Transcript)> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .map(|p| {
            let modified = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, Transcript { path: p })
        })
        .collect();
    entries.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(entries.into_iter().map(|(_, t)| t).collect())
}

/// Resume the latest session (--continue).
pub fn latest(home: &Path) -> Result<Option<Transcript>, TranscriptError> {
    Ok(list(home)?.into_iter().next())
}

impl Transcript {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Session display name: file stem (`{slug}-{ts}` / `{slug}-{ts}-{name}`).
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// Rename session (/rename): `{slug}-{ts}` → `{slug}-{ts}-{name}.jsonl`.
    /// Returns a Transcript pointing at the new path.
    pub fn rename(&self, name: &str) -> Result<Transcript, TranscriptError> {
        let slug = slugify(name);
        if slug.is_empty() || slug == "root" {
            return Err(TranscriptError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty session name",
            )));
        }
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let new_path = self.path.with_file_name(format!("{stem}-{slug}.jsonl"));
        std::fs::rename(&self.path, &new_path)?;
        Ok(Transcript { path: new_path })
    }

    /// Full-file rewrite (persisted after a manual /compact).
    pub fn replace_messages(&self, messages: &[Message]) -> Result<(), TranscriptError> {
        use std::io::Write;
        let mut file = std::fs::File::create(&self.path)?;
        for message in messages {
            let line = serde_json::to_string(message)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    /// Append one message.
    pub fn append(&self, message: &Message) -> Result<(), TranscriptError> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let line = serde_json::to_string(message)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Load all history messages (for --continue resume).
    /// Bad lines are skipped and counted with a warning: one truncated JSONL line must
    /// not make the whole session unrecoverable.
    pub fn load_messages(&self) -> Result<Vec<Message>, TranscriptError> {
        let content = std::fs::read_to_string(&self.path)?;
        let mut messages = Vec::new();
        let mut skipped = 0usize;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(line) {
                Ok(message) => messages.push(message),
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            eprintln!(
                "[bingo] warning: skipped {skipped} unreadable line(s) in {}",
                self.path.display()
            );
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Role;

    #[test]
    fn roundtrip_append_and_load() {
        let tmp = std::env::temp_dir().join("bingo-transcript-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let transcript = create(&home, &tmp).unwrap();
        let msg = Message {
            role: Role::User,
            content: vec![crate::api::types::ContentBlock::Text { text: "hi".into() }],
        };
        transcript.append(&msg).unwrap();
        let messages = transcript.load_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], msg);

        let latest = latest(&home).unwrap().unwrap();
        assert_eq!(latest.load_messages().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// One bad JSONL line must not make the whole session unrecoverable: skip the bad
    /// line, load the rest as usual.
    #[test]
    fn load_skips_corrupt_lines() {
        let tmp = std::env::temp_dir().join(format!("bingo-transcript-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let transcript = create(&home, &tmp).unwrap();
        let good = Message {
            role: Role::User,
            content: vec![crate::api::types::ContentBlock::Text { text: "hi".into() }],
        };
        transcript.append(&good).unwrap();
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(transcript.path())
                .unwrap();
            writeln!(file, "{{\"role\":\"user\",\"content\":[{{\"type\":").unwrap();
            writeln!(file).unwrap();
        }
        transcript.append(&good).unwrap();

        let messages = transcript.load_messages().unwrap();
        assert_eq!(messages.len(), 2, "bad lines skipped, good lines kept");
        assert_eq!(messages[0], good);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn slugifies_odd_names() {
        assert_eq!(slugify("bingo"), "bingo");
        assert_eq!(slugify("a b/c"), "a_b_c");
        assert_eq!(slugify("café"), "caf_");
    }
}
