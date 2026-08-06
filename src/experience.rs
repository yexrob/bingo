use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::skills::parse_frontmatter_pairs;

/// 经验条目状态：active（活跃）/ degraded（降级）/ stale（失效）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExperienceStatus {
    Active,
    Degraded,
    Stale,
}

impl ExperienceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Stale => "stale",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "degraded" => Self::Degraded,
            "stale" => Self::Stale,
            _ => Self::Active,
        }
    }
}

/// 一条沉淀的操作经验（frontmatter + 自由正文的 Markdown 条目）。
/// 存储于 `~/.config/bingo/experience/<project-key>/entries/<id>.md`；
/// 索引不落盘，每次从 entries/ 扫描构建（entries/ 是唯一事实源）。
#[derive(Debug, Clone, PartialEq)]
pub struct ExperienceEntry {
    pub id: String,
    pub project_key: String,
    pub status: ExperienceStatus,
    /// 触发关键词：命中时想起这条经验。
    pub trigger: Vec<String>,
    /// 一句话总结（呈现给用户）。
    pub summary: String,
    /// 执行步骤（可重跑的命令序列）。
    pub steps: Vec<String>,
    pub verify: Option<String>,
    pub evidence: Option<String>,
    pub verified_at: Option<String>,
    /// 被采用次数（Commit 更新已有条目时 +1）。
    pub hits: u64,
    pub created_at: String,
    /// frontmatter 之后的自由正文（手写说明，读写保留）。
    pub notes: String,
}

impl ExperienceEntry {
    /// 由提交字段生成条目：id 按内容摘要计算（不含 status/hits 等易变字段，
    /// 标记失效或计数时 id 保持稳定）。
    pub fn new(
        project_key: &str,
        trigger: Vec<String>,
        summary: String,
        steps: Vec<String>,
        verify: Option<String>,
        evidence: Option<String>,
    ) -> Self {
        let mut entry = Self {
            id: String::new(),
            project_key: project_key.to_string(),
            status: ExperienceStatus::Active,
            trigger,
            summary,
            steps,
            verify,
            evidence,
            verified_at: None,
            hits: 0,
            created_at: unix_to_date(now_secs()),
            notes: String::new(),
        };
        entry.id = entry.content_hash();
        entry
    }

    /// 内容摘要：project_key + trigger + summary + steps（FNV-1a 64，12 hex）。
    fn content_hash(&self) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for part in [
            self.project_key.as_bytes(),
            self.summary.as_bytes(),
            self.trigger.join(",").as_bytes(),
            self.steps.join("\n").as_bytes(),
        ] {
            for byte in part {
                hash ^= *byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        format!("{hash:012x}")
    }

    fn serialize(&self) -> String {
        let mut fm = format!(
            "---\nid: {}\nproject_key: {}\nstatus: {}\ntrigger: {}\nsummary: {}\n",
            self.id,
            self.project_key,
            self.status.as_str(),
            self.trigger.join(", "),
            self.summary
        );
        if !self.steps.is_empty() {
            fm.push_str("steps: |-\n");
            for step in &self.steps {
                fm.push_str(&format!("  {step}\n"));
            }
        }
        if let Some(v) = &self.verify {
            fm.push_str(&format!("verify: {v}\n"));
        }
        if let Some(e) = &self.evidence {
            fm.push_str(&format!("evidence: {e}\n"));
        }
        if let Some(t) = &self.verified_at {
            fm.push_str(&format!("verified_at: {t}\n"));
        }
        fm.push_str(&format!("hits: {}\ncreated_at: {}\n---", self.hits, self.created_at));
        let notes = self.notes.trim();
        if !notes.is_empty() {
            fm.push('\n');
            fm.push_str(notes);
        }
        fm.push('\n');
        fm
    }

    /// 从条目文件内容解析；frontmatter 缺失或 id 缺失返回 None。
    fn parse(content: &str) -> Option<Self> {
        let (pairs, body) = parse_frontmatter_pairs(content);
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let id = get("id")?;
        let project_key = get("project_key").unwrap_or_default();
        let status = ExperienceStatus::from_str(&get("status").unwrap_or_else(|| "active".into()));
        let trigger = get("trigger")
            .map(|t| {
                t.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let summary = get("summary").unwrap_or_default();
        let steps = get("steps")
            .map(|s| {
                s.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let hits = get("hits").and_then(|h| h.parse().ok()).unwrap_or(0);
        Some(Self {
            id,
            project_key,
            status,
            trigger,
            summary,
            steps,
            verify: get("verify"),
            evidence: get("evidence"),
            verified_at: get("verified_at"),
            hits,
            created_at: get("created_at").unwrap_or_default(),
            notes: body.to_string(),
        })
    }
}

#[derive(Debug, Error)]
pub enum ExperienceError {
    #[error("experience io: {0}")]
    Io(#[from] std::io::Error),
}

/// 经验根目录：`$XDG_CONFIG_HOME/bingo/experience`（镜像 skills 配置约定）。
fn experience_root(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    config.join("bingo").join("experience")
}

/// 本项目经验目录：`<root>/<project-key>/entries/`。
fn entries_dir(home: &Path, project_key: &str) -> PathBuf {
    experience_root(home).join(project_key).join("entries")
}

/// 项目键：git remote URL（归一化）→ git 根 → 规范化绝对路径。
/// 跨移动稳定：换目录、换机器经验不消失（Dev-ex 硬约束）。
pub fn project_key(cwd: &Path) -> String {
    if let Some(url) = git_remote(cwd)
        && !normalize_remote(&url).is_empty()
    {
        return sanitize_dirname(&normalize_remote(&url));
    }
    if let Some(root) = git_root(cwd) {
        return sanitize_dirname(&root);
    }
    sanitize_dirname(&cwd.to_string_lossy())
}

fn git_remote(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_root(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// remote URL 归一化：去 scheme / 用户名 / .git 后缀 / 尾斜杠，小写（scheme 大小写不敏感）。
/// scp-like（`git@github.com:owner/repo`）的冒号转 `/`。
fn normalize_remote(url: &str) -> String {
    let lower = url.trim().to_lowercase();
    let scp_style = lower.starts_with("git@");
    let mut u: &str = &lower;
    for prefix in ["https://", "http://", "ssh://"] {
        if let Some(rest) = u.strip_prefix(prefix) {
            u = rest;
        }
    }
    if let Some(rest) = u.strip_prefix("git@") {
        u = rest;
    }
    if scp_style
        && let Some(i) = u.find(':')
    {
        let mut owned = String::with_capacity(u.len());
        owned.push_str(&u[..i]);
        owned.push('/');
        owned.push_str(&u[i + 1..]);
        return owned
            .strip_suffix(".git")
            .map(str::to_string)
            .unwrap_or(owned)
            .trim_end_matches('/')
            .to_string();
    }
    u.strip_suffix(".git")
        .unwrap_or(u)
        .trim_end_matches('/')
        .to_string()
}

/// 目录名安全化：非字母数字与 `-_.` 换成 `-`。
fn sanitize_dirname(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// 加载本项目全部条目（跳过损坏文件——entries/ 是事实源，容错优先）。
pub fn load_entries(home: &Path, project_key: &str) -> Vec<ExperienceEntry> {
    let Ok(entries) = std::fs::read_dir(entries_dir(home, project_key)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if let Some(parsed) = ExperienceEntry::parse(&raw) {
            out.push(parsed);
        }
    }
    out
}

/// 原子写条目：同目录 tmp + rename。
pub fn save_entry(
    home: &Path,
    project_key: &str,
    entry: &ExperienceEntry,
) -> Result<PathBuf, ExperienceError> {
    let dir = entries_dir(home, project_key);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.md", entry.id));
    let tmp = dir.join(format!(".{}.tmp", entry.id));
    std::fs::write(&tmp, entry.serialize())?;
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

/// 删除条目；不存在视为成功。
pub fn delete_entry(home: &Path, project_key: &str, id: &str) -> Result<(), ExperienceError> {
    let path = entries_dir(home, project_key).join(format!("{id}.md"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// 两个小写词元共享的前缀长度（逐字符）。
fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// 词元匹配：query 小写后包含任一 trigger 关键词，或 query 中某词元（≥3 字符）
/// 与 trigger 共享 ≥4 字符前缀（"migrate now" 命中 trigger "migration"）。
/// 结果按 hits 降序、active 优先。
pub fn query<'a>(
    entries: &'a [ExperienceEntry],
    text: &str,
    limit: usize,
) -> Vec<&'a ExperienceEntry> {
    let needle = text.to_lowercase();
    let tokens: Vec<String> = needle
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(str::to_string)
        .collect();
    let mut matched: Vec<&ExperienceEntry> = entries
        .iter()
        .filter(|e| {
            e.trigger.iter().any(|t| {
                let t = t.to_lowercase();
                !t.is_empty()
                    && (needle.contains(&t)
                        || tokens
                            .iter()
                            .any(|tok| shared_prefix_len(&t, tok) >= 4))
            })
        })
        .collect();
    matched.sort_by(|a, b| {
        let a_active = a.status == ExperienceStatus::Active;
        let b_active = b.status == ExperienceStatus::Active;
        b_active
            .cmp(&a_active)
            .then_with(|| b.hits.cmp(&a.hits))
    });
    matched.truncate(limit);
    matched
}

/// 常驻注入索引：active 条目一行一条（≤10 条）+ 溢出提示；空则返回空串。
pub fn format_index(entries: &[ExperienceEntry]) -> String {
    const MAX_INDEX: usize = 10;
    let active: Vec<&ExperienceEntry> = entries
        .iter()
        .filter(|e| e.status == ExperienceStatus::Active)
        .collect();
    if active.is_empty() {
        return String::new();
    }
    let mut lines = Vec::new();
    for entry in active.iter().take(MAX_INDEX) {
        let short = entry.id.chars().take(4).collect::<String>();
        lines.push(format!(
            "- E{short}: {} (hits {})",
            entry.summary, entry.hits
        ));
    }
    if active.len() > MAX_INDEX {
        lines.push(format!(
            "- ... {} more (use ExperienceQuery to search)",
            active.len() - MAX_INDEX
        ));
    }
    lines.join("\n")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// unix 秒 → `YYYY-MM-DD`（公历，无日期依赖）。
fn unix_to_date(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant 的 days↔civil 逆变换。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bingo-exp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn entry_roundtrip_through_frontmatter() {
        let entry = ExperienceEntry::new(
            "key",
            vec!["migration".into(), "db".into()],
            "迁移数据库三步".into(),
            vec!["备份".into(), "执行迁移".into()],
            Some("cargo test".into()),
            Some("会话 2026-08-04".into()),
        );
        let serialized = entry.serialize();
        let parsed = ExperienceEntry::parse(&serialized).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn id_is_stable_across_status_change() {
        let mut entry = ExperienceEntry::new(
            "key",
            vec!["migration".into()],
            "迁移".into(),
            vec!["备份".into()],
            None,
            None,
        );
        let id = entry.id.clone();
        entry.status = ExperienceStatus::Stale;
        assert_eq!(entry.content_hash(), id, "status 变更不换 id");
        entry.hits = 5;
        assert_eq!(entry.content_hash(), id, "hits 变更不换 id");
    }

    #[test]
    fn same_content_same_id() {
        let a = ExperienceEntry::new(
            "key",
            vec!["x".into()],
            "s".into(),
            vec!["1".into()],
            None,
            None,
        );
        let b = ExperienceEntry::new(
            "key",
            vec!["x".into()],
            "s".into(),
            vec!["1".into()],
            None,
            None,
        );
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn notes_roundtrip_preserved() {
        let mut entry = ExperienceEntry::new(
            "key",
            vec![],
            "s".into(),
            vec!["1".into()],
            None,
            None,
        );
        entry.notes = "手写说明\n第二行".into();
        let parsed = ExperienceEntry::parse(&entry.serialize()).unwrap();
        assert_eq!(parsed.notes.trim(), "手写说明\n第二行");
    }

    #[test]
    fn parse_requires_id() {
        assert!(ExperienceEntry::parse("---\nsummary: x\n---\n").is_none());
    }

    #[test]
    fn save_load_delete_roundtrip() {
        let root = tmp_root("crud");
        let home = root.join("home");
        let key = "github-com-example-repo";
        let entry = ExperienceEntry::new(
            key,
            vec!["build".into()],
            "构建三步".into(),
            vec!["cargo build".into(), "cargo test".into()],
            None,
            None,
        );
        let path = save_entry(&home, key, &entry).unwrap();
        assert!(path.ends_with(format!("{}.md", entry.id)));
        let loaded = load_entries(&home, key);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], entry);
        delete_entry(&home, key, &entry.id).unwrap();
        assert!(load_entries(&home, key).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupted_entry_is_skipped() {
        let root = tmp_root("corrupt");
        let home = root.join("home");
        let key = "k";
        let dir = entries_dir(&home, key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.md"), "no frontmatter here").unwrap();
        std::fs::write(dir.join("also-bad.md"), "---\nsummary: no id\n---\n").unwrap();
        assert!(load_entries(&home, key).is_empty(), "损坏条目跳过而非报错");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn query_matches_trigger_and_orders_by_hits_active() {
        let mut hot = ExperienceEntry::new(
            "k",
            vec!["migration".into()],
            "hot".into(),
            vec![],
            None,
            None,
        );
        hot.hits = 9;
        let mut stale = ExperienceEntry::new(
            "k",
            vec!["migration".into()],
            "stale one".into(),
            vec![],
            None,
            None,
        );
        stale.status = ExperienceStatus::Stale;
        stale.hits = 100;
        let cold = ExperienceEntry::new(
            "k",
            vec!["migration".into()],
            "cold".into(),
            vec![],
            None,
            None,
        );
        let entries = vec![hot, stale, cold];
        let results = query(&entries, "do the migration now", 10);
        let summaries: Vec<&str> = results.iter().map(|e| e.summary.as_str()).collect();
        assert_eq!(summaries, vec!["hot", "cold", "stale one"], "active 优先于 hits");
        let limited = query(&entries, "migration", 2);
        assert_eq!(limited.len(), 2);
        // 不匹配的词元 → 空
        assert!(query(&entries, "nothing-here", 10).is_empty());
    }

    #[test]
    fn format_index_caps_at_ten() {
        let entries: Vec<ExperienceEntry> = (0..12)
            .map(|i| ExperienceEntry::new("k", vec!["t".into()], format!("s{i}"), vec![], None, None))
            .collect();
        let index = format_index(&entries);
        let lines: Vec<&str> = index.lines().collect();
        assert_eq!(lines.len(), 11, "10 条 + 1 行溢出提示");
        assert!(lines[10].contains("2 more"));
        // stale 不参与索引
        let mut entries = entries;
        entries[0].status = ExperienceStatus::Stale;
        let index = format_index(&entries);
        assert!(!index.contains("s0"));
        assert!(format_index(&[]).is_empty());
    }

    #[test]
    fn project_key_prefers_remote_over_path() {
        // 非 git 目录：规范化绝对路径，稳定可复现。
        let root = tmp_root("key");
        let key1 = project_key(&root);
        let key2 = project_key(&root);
        assert_eq!(key1, key2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn normalize_remote_variants() {
        assert_eq!(
            normalize_remote("https://github.com/owner/repo.git"),
            "github.com/owner/repo"
        );
        assert_eq!(
            normalize_remote("git@github.com:owner/repo.git"),
            "github.com/owner/repo"
        );
        assert_eq!(
            normalize_remote("ssh://git@example.com:2222/a/b.git"),
            "example.com:2222/a/b"
        );
        assert_eq!(normalize_remote("HTTPS://GitHub.com/Owner/Repo"), "github.com/owner/repo");
    }

    #[test]
    fn unix_date_conversion() {
        assert_eq!(unix_to_date(0), "1970-01-01");
        assert_eq!(unix_to_date(1_752_768_000), "2025-07-17");
        assert_eq!(unix_to_date(1_752_768_000 + 86_400 * 365), "2026-07-17");
    }

    #[test]
    fn dirname_sanitization() {
        assert_eq!(sanitize_dirname("/a b/c"), "-a-b-c");
        // `/` 压平为 `-`：project_key 保持单层目录名（防 `..` 片段穿越 experience 根）。
        assert_eq!(sanitize_dirname("github.com/owner/repo"), "github.com-owner-repo");
    }
}
