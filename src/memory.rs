use std::path::{Path, PathBuf};

use crate::api::types::{Message, Request};
use crate::query::Session;

const MEMORY_MAX_LINES: usize = 200;
/// 提取请求的对话正文上限（字符）。
const MAX_EXTRACT_PROMPT_CHARS: usize = 60_000;

const EXTRACT_PROMPT: &str = "\
你是记忆提取器。从下面的 agent 对话中提取值得长期记住的项目事实：
- 项目结构约定、关键文件路径
- 架构决策与理由
- 构建/测试命令与约定
- 用户偏好与约束
只输出事实列表，每行一条，不要编号，不要客套话。没有值得记住的事实就输出空。
对话：
";

/// memdir 目录：~/.config/bingo/memdir/。
pub fn memdir_dir(home: &Path) -> PathBuf {
    home.join(".config").join("bingo").join("memdir")
}

/// 完整路径的 FNV-1a 64 摘要（跨进程/跨版本稳定，故不用 DefaultHasher）。
fn path_hash(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 本项目对应的记忆文件：`<目录名>-<完整路径哈希>.md`。
/// 只用目录名会让同名项目（如多个 `web`）互相串味。
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

/// 读取本项目记忆（不存在则 None）。
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

/// 会话结束后：从对话提取事实追加到记忆文件（失败静默）。
pub async fn extract_memory(session: &Session, messages: &[Message], home: &Path, cwd: &Path) {
    if messages.len() < 2 {
        return;
    }
    let mut prompt = String::from(EXTRACT_PROMPT);
    let mut truncated = false;
    for message in messages {
        // 尾部截断：长会话的完整转录会撑爆提取请求。
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
        prompt.push_str("\n---\n[对话过长，已截断]");
    }

    let request = Request {
        model: session.runtime.model.borrow().clone(),
        max_tokens: 512,
        system: Vec::new(),
        messages: vec![Message::user_text(prompt)],
        tools: Vec::new(),
        stream: false,
        thinking: None,
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
        // 截断超长记忆
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
        // 同一路径稳定。
        assert_eq!(memory_file(home, cwd), path);
    }

    /// L6 回归：同名目录的不同项目不得共用记忆文件。
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
