use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::api::types::Message;

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("failed to write transcript: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse transcript line: {0}")]
    Parse(#[from] serde_json::Error),
}

/// 会话 transcript：JSONL 逐行一条 Message（D11）。
#[derive(Debug, Clone)]
pub struct Transcript {
    path: PathBuf,
}

fn slugify(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() {
        "root".to_string()
    } else {
        cleaned
    }
}

/// transcripts 目录：~/.local/share/bingo/transcripts。
pub fn transcripts_dir(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("bingo").join("transcripts")
}

/// 新建会话文件：<project-slug>-<unix-ts>.jsonl。
pub fn create(home: &Path, cwd: &Path) -> Result<Transcript, TranscriptError> {
    let dir = transcripts_dir(home);
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = cwd.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let slug = slugify(&name);
    let path = dir.join(format!("{slug}-{ts}.jsonl"));
    Ok(Transcript { path })
}

/// 恢复最新会话（--continue）。
pub fn latest(home: &Path) -> Result<Option<Transcript>, TranscriptError> {
    let dir = transcripts_dir(home);
    if !dir.exists() {
        return Ok(None);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    entries.sort_by_key(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    Ok(entries.last().cloned().map(|path| Transcript { path }))
}

impl Transcript {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 追加一条消息。
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

    /// 读取全部历史消息（--continue 恢复用）。
    pub fn load_messages(&self) -> Result<Vec<Message>, TranscriptError> {
        let content = std::fs::read_to_string(&self.path)?;
        content
            .lines()
            .map(|line| Ok(serde_json::from_str::<Message>(line)?))
            .collect()
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

    #[test]
    fn slugifies_odd_names() {
        assert_eq!(slugify("bingo"), "bingo");
        assert_eq!(slugify("a b/c"), "a_b_c");
        assert_eq!(slugify("你好"), "__");
    }
}
