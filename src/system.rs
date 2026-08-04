use std::path::Path;

use crate::api::types::SystemBlock;

const BASE_PROMPT: &str = "\
You are bingo, an agent CLI running on the user's machine.

Rules:
- Use tools to accomplish the user's task; do not claim to have done things you cannot do.
- Prefer reading files before editing them. Prefer searching over guessing file locations.
- When a tool fails or is denied, report the failure honestly and adapt.
- When the task is complete, stop and summarize concisely.
";

/// 记忆层级：user + project CLAUDE.md。
#[derive(Debug, Default)]
pub struct Memory {
    pub user: Option<String>,
    pub project: Option<String>,
}

fn read_opt(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 加载记忆层（对标 Claude Code 层级：user → project）。
pub fn load_memory(home: &Path, cwd: &Path) -> Memory {
    let user = read_opt(&home.join(".claude").join("CLAUDE.md"));
    let project = [cwd.join("CLAUDE.md"), cwd.join(".claude").join("CLAUDE.md")]
        .iter()
        .find_map(|p| read_opt(p));
    Memory { user, project }
}

/// 拼装 system prompt：base 段始终在前；记忆段随文件存在与否增减。
/// `cache_control` 控制是否发送 cache_control（默认关闭，非官方端点不稳定）。
pub fn build_system(
    memory: &Memory,
    project_memory: Option<String>,
    cache_control: bool,
) -> Vec<SystemBlock> {
    let block = |text: String| SystemBlock { text, cache: cache_control };
    let mut blocks = vec![block(BASE_PROMPT.to_string())];
    if let Some(user) = &memory.user {
        blocks.push(block(format!("User-level memory (CLAUDE.md):\n{user}")));
    }
    if let Some(project) = &memory.project {
        blocks.push(block(format!("Project-level memory (CLAUDE.md):\n{project}")));
    }
    if let Some(mem) = project_memory {
        blocks.push(block(format!("Persistent project memory (auto-extracted):\n{mem}")));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_blocks_with_base_first() {
        let memory = Memory {
            user: Some("user rules".into()),
            project: Some("project rules".into()),
        };
        let blocks = build_system(&memory, Some("mem facts".into()), true);
        assert_eq!(blocks.len(), 4);
        assert!(blocks[3].text.contains("mem facts"));
        assert!(blocks[0].text.starts_with("You are bingo"));
        assert!(blocks[1].text.contains("user rules"));
        assert!(blocks[2].text.contains("project rules"));
        assert!(blocks.iter().all(|b| b.cache));
    }

    #[test]
    fn cache_control_off_by_default() {
        let blocks = build_system(&Memory::default(), None, false);
        assert!(blocks.iter().all(|b| !b.cache));
    }

    #[test]
    fn omits_missing_memory() {
        let memory = Memory::default();
        let blocks = build_system(&memory, None, true);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn skips_empty_memory_files() {
        let tmp = std::env::temp_dir().join("bingo-memory-test");
        let _ = std::fs::create_dir_all(tmp.join(".claude"));
        std::fs::write(tmp.join("CLAUDE.md"), "  \n").unwrap();
        let memory = load_memory(&tmp, &tmp);
        assert!(memory.project.is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
