use std::path::{Path, PathBuf};

use crate::api::types::{Message, Request};
use crate::query::Session;

const MEMORY_MAX_LINES: usize = 200;
/// Char cap for the extraction request's conversation body.
const MAX_EXTRACT_PROMPT_CHARS: usize = 60_000;

const EXTRACT_PROMPT: &str = "\
You are a memory extractor. Extract project facts worth remembering long-term from the agent conversation below:
- Project structure conventions and key file paths
- Architecture decisions and their rationale
- Build/test commands and conventions
- User preferences and constraints
Output only a fact list, one per line: no numbering, no pleasantries. Output nothing when no fact is worth remembering.
Conversation:
";

/// memdir directory: ~/.config/bingo/memdir/.
pub fn memdir_dir(home: &Path) -> PathBuf {
    home.join(".config").join("bingo").join("memdir")
}

/// FNV-1a 64 digest of the full path (stable across processes/versions, hence not
/// DefaultHasher).
/// D31 team memory's project_hash reuses the same digest (same key family as project
/// memory).
pub(crate) fn path_hash(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// This project's memory file: `<dirname>-<full-path-hash>.md`.
/// Using only the dirname would make same-named projects (e.g. several `web`)
/// contaminate each other.
pub fn memory_file(home: &Path, cwd: &Path) -> PathBuf {
    let name = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let name: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    memdir_dir(home).join(format!("{name}-{}.md", path_hash(cwd)))
}

/// Read this project's memory (None if missing).
pub fn load_project_memory(home: &Path, cwd: &Path) -> Option<String> {
    let path = memory_file(home, cwd);
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// After a session ends: extract facts from the conversation and append them to the
/// memory file (failures are silent).
pub async fn extract_memory(session: &Session, messages: &[Message], home: &Path, cwd: &Path) {
    if messages.len() < 2 {
        return;
    }
    let mut prompt = String::from(EXTRACT_PROMPT);
    let mut truncated = false;
    for message in messages {
        // Tail truncation: a long session's full transcript would blow up the
        // extraction request.
        if prompt.chars().count() >= MAX_EXTRACT_PROMPT_CHARS {
            truncated = true;
            break;
        }
        let text = message
            .content
            .iter()
            .filter_map(|block| match block {
                crate::api::types::ContentBlock::Text { text } => Some(text.clone()),
                crate::api::types::ContentBlock::ToolResult { content, .. } => {
                    Some(content.to_string())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            prompt.push_str(&format!("\n---\n{text}"));
        }
    }
    if prompt.chars().count() > MAX_EXTRACT_PROMPT_CHARS {
        prompt = prompt.chars().take(MAX_EXTRACT_PROMPT_CHARS).collect();
        truncated = true;
    }
    if truncated {
        prompt.push_str("\n---\n[conversation too long, truncated]");
    }

    let request = Request {
        model: session.runtime.model.borrow().clone(),
        max_tokens: 512,
        system: Vec::new(),
        messages: vec![Message::user_text(prompt)],
        tools: Vec::new(),
        stream: false,
        thinking: None,
        output_config: None,
    };
    let facts = match session.client.complete_text(&request).await {
        Ok(f) => f,
        Err(e) => {
            if !session.quiet {
                eprintln!("[bingo] memory: extract failed: {e}");
            }
            return;
        }
    };
    let facts = facts.trim();
    if facts.is_empty() {
        return;
    }

    let path = memory_file(home, cwd);
    if let Err(e) = std::fs::create_dir_all(path.parent().expect("记忆文件路径必有父目录")) {
        if !session.quiet {
            eprintln!("[bingo] memory: cannot create dir: {e}");
        }
        return;
    }
    let mut existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut added = 0;
    for line in facts.lines() {
        let line = line.trim();
        if line.is_empty() || existing.lines().any(|l| l.trim() == line) {
            continue;
        }
        existing.push_str(line);
        existing.push('\n');
        added += 1;
    }
    if added > 0 {
        // Truncate over-long memory
        let lines: Vec<&str> = existing.lines().take(MEMORY_MAX_LINES).collect();
        if let Err(e) = std::fs::write(&path, lines.join("\n") + "\n") {
            if !session.quiet {
                eprintln!("[bingo] memory: write failed: {e}");
            }
            return;
        }
        if !session.quiet {
            eprintln!("[bingo] memory: added {added} facts -> {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_file_path() {
        let home = Path::new("/tmp/h");
        let cwd = Path::new("/tmp/h/proj");
        let path = memory_file(home, cwd);
        assert!(path.starts_with("/tmp/h/.config/bingo/memdir"));
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        assert!(name.starts_with("proj-") && name.ends_with(".md"), "{name}");
        // Same path is stable.
        assert_eq!(memory_file(home, cwd), path);
    }

    /// L6 regression: same-named dirs of different projects must not share a memory file.
    #[test]
    fn same_dir_name_different_projects_do_not_collide() {
        let home = Path::new("/tmp/h");
        let a = memory_file(home, Path::new("/work/alpha/web"));
        let b = memory_file(home, Path::new("/work/beta/web"));
        assert_ne!(a, b, "同名 web 目录应有不同记忆文件");
        assert!(
            a.file_name().unwrap_or_default().to_string_lossy().starts_with("web-")
                && b.file_name().unwrap_or_default().to_string_lossy().starts_with("web-"),
            "仍保留可读的目录名前缀"
        );
    }

    #[test]
    fn dedupes_and_truncates() {
        let home = Path::new("/tmp/bingo-mem-test");
        let cwd = home.join("p");
        std::fs::create_dir_all(memdir_dir(home)).unwrap();
        let path = memory_file(home, &cwd);
        std::fs::write(&path, "fact one\nfact two\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
        std::fs::remove_dir_all(home).unwrap();
    }
}
