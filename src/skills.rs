use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 一个技能：`<name>/SKILL.md`（YAML frontmatter + markdown 正文）。
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    /// frontmatter `arguments:` 声明的参数名（`$name` 按位置替换）。
    pub argument_names: Vec<String>,
    /// SKILL.md 所在目录（`${CLAUDE_SKILL_DIR}` 与相对引用基准）。
    pub base_dir: PathBuf,
    /// frontmatter 剥离后的正文。
    pub content: String,
}

/// frontmatter 解析结果（行解析的简单形状；
/// 仅支持 `key: value` 单行，不做完整 YAML）。
#[derive(Debug, Default, PartialEq)]
pub struct Frontmatter {
    pub description: Option<String>,
    pub when_to_use: Option<String>,
    pub argument_names: Vec<String>,
}

/// 解析 `---\nkey: value\n---` 前置块为键值对 + 正文；无 frontmatter 时
/// 返回空对 + 原文。任何键都支持 YAML 折叠/字面标量（`>-` / `|` 等）：
/// 后续缩进行并入值（`|` 系保留换行，`>` 系折叠为空格）。
/// 技能与 agent 定义共用（各自再解释键的语义）。
pub fn parse_frontmatter_pairs(content: &str) -> (Vec<(String, String)>, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        return (Vec::new(), content);
    };
    let Some(end) = rest.find("\n---") else {
        return (Vec::new(), content);
    };
    let mut pairs = Vec::new();
    let fm_lines: Vec<&str> = rest[..end].lines().collect();
    let mut i = 0;
    while i < fm_lines.len() {
        let line = fm_lines[i];
        let Some((key, value)) = line.split_once(':') else {
            i += 1;
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if matches!(value, ">" | ">-" | "|" | "|-") {
            let mut parts: Vec<&str> = Vec::new();
            i += 1;
            while i < fm_lines.len() {
                let cont = fm_lines[i];
                if cont.starts_with(' ') || cont.is_empty() {
                    parts.push(cont.trim());
                    i += 1;
                } else {
                    break;
                }
            }
            let joined = if value.starts_with('|') {
                parts.join("\n")
            } else {
                parts.join(" ")
            };
            let joined = joined.trim();
            if !joined.is_empty() {
                pairs.push((key.to_string(), joined.to_string()));
            }
            continue;
        }
        if !value.is_empty() {
            pairs.push((key.to_string(), value.to_string()));
        }
        i += 1;
    }
    let body = rest[end + 4..].trim_start();
    (pairs, body)
}

/// 技能视角的 frontmatter：description / when_to_use / arguments。
pub fn parse_frontmatter(content: &str) -> (Frontmatter, &str) {
    let (pairs, body) = parse_frontmatter_pairs(content);
    let mut fm = Frontmatter::default();
    for (key, value) in pairs {
        match key.as_str() {
            "description" => fm.description = Some(value),
            "when_to_use" => fm.when_to_use = Some(value),
            // CC 支持空格分隔或数组；此处统一逗号/空格分隔。
            "arguments" => {
                fm.argument_names = value
                    .split([',', ' '])
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()))
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    (fm, body)
}

/// description 缺失时的回落：正文第一个非空行。
pub fn first_line(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// 用户级技能目录：`$XDG_CONFIG_HOME/bingo/skills`（镜像 main.rs 的配置约定）。
fn user_skills_dir(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    config.join("bingo").join("skills")
}

/// 从 cwd 向上逐层找 `.bingo/skills`。
fn project_skills_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        dirs.push(d.join(".bingo").join("skills"));
        dir = d.parent();
    }
    dirs
}

fn load_dir(dir: &Path, out: &mut Vec<Skill>) {
    let Ok(mut entries) = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().collect::<Vec<_>>())
    else {
        return;
    };
    // readdir 顺序不保证（APFS 任意序）：按名排序，让清单可预期。
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let skill_dir = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(raw) = std::fs::read_to_string(skill_dir.join("SKILL.md")) else {
            continue;
        };
        let (fm, body) = parse_frontmatter(&raw);
        let description = fm
            .description
            .or_else(|| Some(first_line(body)).filter(|d| !d.is_empty()))
            .unwrap_or_default();
        out.push(Skill {
            name,
            description,
            when_to_use: fm.when_to_use,
            argument_names: fm.argument_names,
            base_dir: skill_dir,
            content: body.to_string(),
        });
    }
}

/// 内置技能清单（编译进二进制）：
/// 每个技能的内容内嵌在 `bundled/*.md`，base_dir 为空（无文件基准）。
pub fn bundled_skills() -> Vec<Skill> {
    let mut skills = Vec::new();
    let (name, raw) = ("guide", include_str!("skills/bundled/guide.md"));
    let (fm, body) = parse_frontmatter(raw);
    skills.push(Skill {
        name: name.to_string(),
        description: fm
            .description
            .or_else(|| Some(first_line(body)).filter(|d| !d.is_empty()))
            .unwrap_or_default(),
        when_to_use: fm.when_to_use,
        argument_names: fm.argument_names,
        base_dir: PathBuf::new(),
        content: body.to_string(),
    });
    skills
}

/// 扫描指纹：技能目录与已加载 SKILL.md 的 mtime。
/// 目录 mtime 捕获增删，文件 mtime 捕获内容改动。
type Stamps = Vec<(PathBuf, Option<std::time::SystemTime>)>;

fn stamp(path: &Path) -> (PathBuf, Option<std::time::SystemTime>) {
    (
        path.to_path_buf(),
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok()),
    )
}

struct SkillCache {
    key: (PathBuf, PathBuf),
    stamps: Stamps,
    skills: Vec<Skill>,
}

/// 进程内缓存：每回合全量重扫技能目录（用户级技能可上百个）是纯浪费。
static SKILL_CACHE: std::sync::LazyLock<std::sync::Mutex<Option<SkillCache>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

fn scan_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![user_skills_dir(home)];
    dirs.extend(project_skills_dirs(cwd));
    dirs
}

/// 加载全部技能：内置（编译期）→ user 层 → 项目层（近 cwd 优先）；
/// 按名字去重，磁盘技能覆盖同名内置（用户自定义优先）。
/// 目录/文件 mtime 未变时复用上一次扫描结果。
pub fn load_skills(home: &Path, cwd: &Path) -> Vec<Skill> {
    let key = (home.to_path_buf(), cwd.to_path_buf());
    let dirs = scan_dirs(home, cwd);
    let dir_stamps: Stamps = dirs.iter().map(|d| stamp(d)).collect();

    let mut cache = SKILL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = cache.as_ref()
        && cached.key == key
        && cached.stamps.len() >= dir_stamps.len()
        && cached.stamps[..dir_stamps.len()] == dir_stamps[..]
        && cached.stamps[dir_stamps.len()..]
            .iter()
            .all(|(path, at)| stamp(path).1 == *at)
    {
        return cached.skills.clone();
    }

    let mut skills = Vec::new();
    for dir in &dirs {
        load_dir(dir, &mut skills);
    }
    let mut seen = HashSet::new();
    skills.retain(|s| seen.insert(realpath_or(&s.base_dir.join("SKILL.md"))));
    let mut stamps = dir_stamps;
    stamps.extend(skills.iter().map(|s| stamp(&s.base_dir.join("SKILL.md"))));
    // 内置技能排在磁盘技能之后：同名时磁盘（先入）胜出。
    let names: HashSet<String> = skills.iter().map(|s| s.name.clone()).collect();
    skills.extend(
        bundled_skills()
            .into_iter()
            .filter(|b| !names.contains(&b.name)),
    );
    *cache = Some(SkillCache {
        key,
        stamps,
        skills: skills.clone(),
    });
    skills
}

fn realpath_or(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// 按参数名/位置替换占位符：
/// `$ARGUMENTS` → 完整参数串；`$ARGUMENTS[N]`/`$N` → 第 N 个参数；
/// `$name` → 按声明顺序的第 N 个参数（后不跟 `[` 或单词字符）。
/// args 为空串时占位符替换为空；无占位符且 args 非空时追加 `ARGUMENTS:`。
pub fn substitute_arguments(content: &str, args: &str, argument_names: &[String]) -> String {
    let parsed: Vec<&str> = args.split_whitespace().collect();
    let mut out = content.to_string();
    for (i, name) in argument_names.iter().enumerate() {
        let value = parsed.get(i).copied().unwrap_or("");
        let needle = format!("${name}");
        out = replace_word_boundary(&out, &needle, value);
    }
    let full = args.to_string();
    let indexed: Vec<(String, String)> = (0..parsed.len())
        .map(|i| (format!("$ARGUMENTS[{i}]"), parsed[i].to_string()))
        .collect();
    for (needle, value) in &indexed {
        out = out.replace(needle, value);
    }
    // $N 简写：从大到小替换避免 "$10" 被 "$1" 截胡；后不跟单词字符/`[`。
    for (i, value) in parsed.iter().enumerate().rev() {
        let needle = format!("${i}");
        out = replace_word_boundary(&out, &needle, value);
    }
    let no_full = !out.contains("$ARGUMENTS");
    out = out.replace("$ARGUMENTS", &full);
    if no_full && !args.is_empty() {
        out.push_str(&format!("\n\nARGUMENTS: {args}"));
    }
    out
}

fn replace_word_boundary(haystack: &str, needle: &str, value: &str) -> String {
    let mut out = String::new();
    let mut rest = haystack;
    while let Some(pos) = rest.find(needle) {
        let after = &rest[pos + needle.len()..];
        let boundary = after.chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '['));
        if boundary {
            out.push_str(&rest[..pos]);
            out.push_str(value);
            rest = after;
        } else {
            out.push_str(&rest[..pos + needle.len()]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// 技能展开：`Base directory for this skill: {dir}` 头（内置技能无文件基准时省略）
/// + 参数替换 + `${CLAUDE_SKILL_DIR}`。
pub fn expand_skill(skill: &Skill, args: &str) -> String {
    let mut content = skill.content.clone();
    if !skill.base_dir.as_os_str().is_empty() {
        content = format!(
            "Base directory for this skill: {}\n\n{}",
            skill.base_dir.display(),
            content
        );
    }
    content = substitute_arguments(&content, args, &skill.argument_names);
    content.replace("${CLAUDE_SKILL_DIR}", &skill.base_dir.display().to_string())
}

/// 清单条目截断长度。
pub const MAX_LISTING_DESC_CHARS: usize = 250;
/// 清单默认字符预算（上下文 1%）。
pub const DEFAULT_CHAR_BUDGET: usize = 8000;

fn listing_entry(skill: &Skill) -> String {
    let mut desc = skill.description.clone();
    if let Some(when) = &skill.when_to_use {
        desc.push_str(" - ");
        desc.push_str(when);
    }
    if desc.chars().count() > MAX_LISTING_DESC_CHARS {
        let cut: String = desc.chars().take(MAX_LISTING_DESC_CHARS - 1).collect();
        desc = format!("{cut}…");
    }
    format!("- {}: {desc}", skill.name)
}

/// 预算内按序生成清单；预算不足时完整条目放不下的技能
/// 退化为 `- name` 纯名单行——技能名必须全量可见，
/// 否则模型会误判技能不存在（如上百个技能时 meye 被截掉）。
/// 名字的占用先预留，完整条目只吃剩余预算；预算小到名字
/// 都放不下时才尽力截断（硬预算）。
pub fn format_listing(skills: &[Skill], budget: usize) -> String {
    let mut out = String::new();
    // 全部名字的纯名单行占用（`- name\n`）。
    let names_min = skills
        .iter()
        .map(|s| s.name.len() + 3)
        .sum::<usize>()
        .saturating_sub(1);
    if names_min > budget {
        for skill in skills {
            let entry = format!("- {}", skill.name);
            if !fits_in(&out, &entry, budget) {
                break;
            }
            push_line(&mut out, &entry);
        }
        return out;
    }
    let desc_cap = budget - names_min;
    let mut listed: Vec<&str> = Vec::new();
    for skill in skills {
        let entry = listing_entry(skill);
        if fits_in(&out, &entry, desc_cap) {
            listed.push(skill.name.as_str());
            push_line(&mut out, &entry);
        }
    }
    for skill in skills {
        if listed.contains(&skill.name.as_str()) {
            continue;
        }
        let entry = format!("- {}", skill.name);
        if !fits_in(&out, &entry, budget) {
            break;
        }
        push_line(&mut out, &entry);
    }
    out
}

/// 条目能否放入预算；清单为空时首条必放（首条超预算不截断）。
fn fits_in(out: &str, entry: &str, budget: usize) -> bool {
    out.is_empty() || out.len() + entry.len() < budget
}

fn push_line(out: &mut String, entry: &str) {
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn parses_frontmatter_fields() {
        let (fm, body) = parse_frontmatter(
            "---\ndescription: Review a PR\ntype: test\nwhen_to_use: After opening a PR\narguments: diff base\n---\n# Body\ncontent here\n",
        );
        assert_eq!(fm.description.as_deref(), Some("Review a PR"));
        assert_eq!(fm.when_to_use.as_deref(), Some("After opening a PR"));
        assert_eq!(fm.argument_names, vec!["diff".to_string(), "base".to_string()]);
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn missing_frontmatter_keeps_content() {
        let (fm, body) = parse_frontmatter("plain text\n");
        assert_eq!(fm, Frontmatter::default());
        assert_eq!(body, "plain text\n");
    }

    #[test]
    fn parses_folded_and_literal_scalars() {
        // `>-` 折叠：后续缩进行以空格连接。
        let (fm, body) = parse_frontmatter(
            "---\ndescription: >-\n  Entry point for the\n  Meye screen-capture app.\n---\n# Body\n",
        );
        assert_eq!(
            fm.description.as_deref(),
            Some("Entry point for the Meye screen-capture app.")
        );
        assert!(body.starts_with("# Body"));
        // `|-` 字面：保留换行。
        let (fm, _) = parse_frontmatter("---\ndescription: |-\n  line one\n  line two\n---\n");
        assert_eq!(fm.description.as_deref(), Some("line one\nline two"));
        // 折叠块结束于 `---` 前，正文完整。
        let (fm, body) = parse_frontmatter("---\nwhen_to_use: >\n  after a PR\n---\nrest\n");
        assert_eq!(fm.when_to_use.as_deref(), Some("after a PR"));
        assert!(body.starts_with("rest"));
    }

    #[test]
    fn description_falls_back_to_first_body_line() {
        let root = std::env::temp_dir().join(format!("bingo-skills-{}-desc", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/skills/pdf/SKILL.md"),
            "---\nname: pdf\n---\nConverts documents to PDF.\nMore text.\n",
        );
        let skills = load_skills(&home, &root);
        let pdf = skills.iter().find(|s| s.name == "pdf").unwrap();
        assert_eq!(pdf.description, "Converts documents to PDF.");
        assert!(pdf.content.contains("More text."));
        assert!(pdf.content.find("---").is_none(), "frontmatter 已剥离");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn loads_bundled_user_and_project_with_dedup() {
        let root = std::env::temp_dir().join(format!("bingo-skills-{}-load", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("project");
        let nested = project.join("src");
        write(
            &home.join(".config/bingo/skills/one/SKILL.md"),
            "---\ndescription: user one\n---\nuser body\n",
        );
        write(
            &project.join(".bingo/skills/two/SKILL.md"),
            "---\ndescription: project two\n---\nproject body\n",
        );
        write(
            &nested.join(".bingo/skills/three/SKILL.md"),
            "---\ndescription: nested three\n---\nnested body\n",
        );
        std::fs::create_dir_all(&nested).unwrap();

        let skills = load_skills(&home, &nested);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["one", "three", "two", "guide"],
            "磁盘技能优先 + 内置技能兜底"
        );

        // 同源文件（SKILL.md 经符号链接指向 user 层）按 realpath 去重 first-wins。
        #[cfg(unix)]
        {
            std::fs::create_dir_all(project.join(".bingo/skills/one")).unwrap();
            std::os::unix::fs::symlink(
                home.join(".config/bingo/skills/one/SKILL.md"),
                project.join(".bingo/skills/one/SKILL.md"),
            )
            .unwrap();
            let skills = load_skills(&home, &project);
            let count = skills.iter().filter(|s| s.name == "one").count();
            assert_eq!(count, 1, "同源文件去重");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// L2：缓存命中不改变结果，目录/文件变动后失效重扫。
    #[test]
    fn cached_scan_invalidates_on_change() {
        let root = std::env::temp_dir().join(format!("bingo-skills-{}-cache", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/skills/one/SKILL.md"),
            "---\ndescription: first\n---\nbody\n",
        );
        let first = load_skills(&home, &root);
        // 缓存命中：同样的输入给同样的结果。
        let cached = load_skills(&home, &root);
        assert_eq!(
            first.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            cached.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
        );
        let desc = |skills: &[Skill], name: &str| {
            skills
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.description.clone())
                .unwrap_or_default()
        };
        assert_eq!(desc(&cached, "one"), "first");

        // mtime 分辨率兜底：确保改动落在不同的时间戳上。
        std::thread::sleep(std::time::Duration::from_millis(50));
        // 内容改动（目录 mtime 不变）也要失效。
        write(
            &home.join(".config/bingo/skills/one/SKILL.md"),
            "---\ndescription: second\n---\nbody\n",
        );
        assert_eq!(desc(&load_skills(&home, &root), "one"), "second", "内容改动应失效");

        std::thread::sleep(std::time::Duration::from_millis(50));
        // 新增技能目录也要失效。
        write(
            &home.join(".config/bingo/skills/two/SKILL.md"),
            "---\ndescription: added\n---\nbody\n",
        );
        let after = load_skills(&home, &root);
        assert!(after.iter().any(|s| s.name == "two"), "新增技能应被看到");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundled_skill_overridden_by_disk_skill() {
        let root = std::env::temp_dir().join(format!("bingo-skills-{}-bundle", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/skills/guide/SKILL.md"),
            "---\ndescription: custom guide\n---\ncustom body\n",
        );
        let skills = load_skills(&home, &root);
        let self_doc = skills.iter().find(|s| s.name == "guide").unwrap();
        assert_eq!(self_doc.description, "custom guide", "磁盘技能覆盖内置");
        assert!(!self_doc.content.contains("诊断指南"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bundled_expand_omits_base_dir_header() {
        let skill = bundled_skills()
            .into_iter()
            .find(|s| s.name == "guide")
            .unwrap();
        let out = expand_skill(&skill, "");
        assert!(!out.starts_with("Base directory for this skill:"), "{out}");
        assert!(out.contains("诊断指南"), "内置内容完整");
    }

    #[test]
    fn substitutes_arguments_like_reference() {
        let content = "Do $ARGUMENTS[0] on $1 then $ARGUMENTS with $msg";
        let out = substitute_arguments(content, "fix bug", &["msg".to_string()]);
        assert_eq!(
            out,
            "Do fix on bug then fix bug with fix",
            "named 映射首个位置，$1 与 $ARGUMENTS 按同一语义"
        );

        let no_placeholder = substitute_arguments("plain", "a b", &[]);
        assert!(no_placeholder.ends_with("ARGUMENTS: a b"), "{no_placeholder}");

        let empty = substitute_arguments("$ARGUMENTS", "", &[]);
        assert_eq!(empty, "");
    }

    #[test]
    fn expand_adds_base_dir_header_and_substitutes_skill_dir() {
        let skill = Skill {
            name: "pdf".into(),
            description: "d".into(),
            when_to_use: None,
            argument_names: vec![],
            base_dir: PathBuf::from("/tmp/skill-dir"),
            content: "run ${CLAUDE_SKILL_DIR}/build.sh".into(),
        };
        let out = expand_skill(&skill, "");
        assert!(out.starts_with("Base directory for this skill: /tmp/skill-dir\n\n"));
        assert!(out.contains("/tmp/skill-dir/build.sh"));
    }

    #[test]
    fn listing_respects_budget_and_desc_cap() {
        let skill = |name: &str, desc: &str| Skill {
            name: name.into(),
            description: desc.into(),
            when_to_use: None,
            argument_names: vec![],
            base_dir: PathBuf::new(),
            content: String::new(),
        };
        let long = "x".repeat(300);
        let listing = format_listing(&[skill("a", &long)], 8000);
        assert!(listing.contains("…"), "单条超 250 字符截断");

        let short = format_listing(&[skill("a", "aa"), skill("b", "bb")], 10);
        assert_eq!(short, "- a: aa", "超预算即停（名字也放不下时）");
    }

    /// 预算不足时完整条目放不下的技能退化为纯名单行：
    /// 每个技能名必须出现，否则模型会误判技能不存在。
    #[test]
    fn listing_never_drops_skill_names() {
        let skill = |name: &str, desc: &str| Skill {
            name: name.into(),
            description: desc.into(),
            when_to_use: None,
            argument_names: vec![],
            base_dir: PathBuf::new(),
            content: String::new(),
        };
        // 40 个长描述技能：完整条目约 10KB，远超 8000 预算。
        let skills: Vec<Skill> = (0..40)
            .map(|i| skill(&format!("skill-{i:02}"), &"d".repeat(300)))
            .collect();
        let listing = format_listing(&skills, 8000);
        for s in &skills {
            assert!(
                listing.contains(&format!("- {}", s.name)),
                "预算不足也不能丢技能名: {}",
                s.name
            );
        }
        assert!(listing.len() <= 8000, "硬预算仍生效: {}", listing.len());
        // 完整描述条目在前，纯名单兜底在后。
        let head = listing.lines().next().unwrap();
        assert!(head.starts_with("- skill-00: "), "完整条目在前: {head}");
    }

    /// 同目录技能按名排序：readdir 顺序不保证，清单必须确定性。
    #[test]
    fn load_dir_sorts_by_name() {
        let root = std::env::temp_dir().join(format!("bingo-skills-{}-sort", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        for name in ["zeta", "alpha", "meye"] {
            write(
                &home.join(format!(".config/bingo/skills/{name}/SKILL.md")),
                &format!("---\ndescription: {name} desc\n---\nbody\n"),
            );
        }
        let skills = load_skills(&home, &root);
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "meye", "zeta", "guide"], "按名排序");
        let _ = std::fs::remove_dir_all(&root);
    }
}
