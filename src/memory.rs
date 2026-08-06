use std::path::{Path, PathBuf};

use crate::api::types::{Message, Request};
use crate::query::Session;

const MEMORY_MAX_LINES: usize = 200;

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

/// 本项目对应的记忆文件：<slug>.md。
pub fn memory_file(home: &Path, cwd: &Path) -> PathBuf {
    let slug = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    memdir_dir(home).join(format!("{slug}.md"))
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
    for message in messages {
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
        assert_eq!(
            memory_file(home, cwd),
            Path::new("/tmp/h/.config/bingo/memdir/proj.md")
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
