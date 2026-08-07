use async_trait::async_trait;
use serde::Deserialize;

use crate::skills::{expand_skill, format_listing, Skill, DEFAULT_CHAR_BUDGET};

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// Skill tool input.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SkillInput {
    #[schemars(description = "The skill name (e.g. \"commit\", \"review-pr\")")]
    pub skill: String,
    #[serde(default)]
    #[schemars(description = "Optional arguments for the skill")]
    pub args: Option<String>,
}

/// Skill: execute a skill by name from the skills registry.
/// Returns a single line `✦ {name} — read <SKILL.md path>` (disk skills point at their own
/// file; bundled skills are materialized into the cache directory); the model Reads it for
/// full instructions; only if materialization fails, expand the full content inline as a fallback.
pub struct SkillTool {
    skills: Vec<Skill>,
}

impl SkillTool {
    pub fn new(skills: Vec<Skill>) -> Self {
        Self { skills }
    }

    fn find(&self, name: &str) -> Option<&Skill> {
        let name = name.strip_prefix('/').unwrap_or(name);
        self.skills.iter().find(|s| s.name == name)
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> String {
        "Skill".into()
    }
    fn description(&self) -> String {
        let listing = format_listing(&self.skills, DEFAULT_CHAR_BUDGET);
        let mut desc = "Execute a skill within the main conversation

When users ask you to perform tasks, check if any of the available skills match. Skills provide specialized capabilities and domain knowledge.

When users reference a slash command or \"/<something>\" (e.g. \"/commit\"), or use a \"✦ name\" marker (e.g. \"✦ meye\"), they are referring to a skill. Use this tool to invoke it.

Invoking returns a pointer to the skill's SKILL.md (\"✦ name — read <path>\"); read that file with Read to get the full instructions.

IMPORTANT: When a skill matches the user's request, invoke the Skill tool BEFORE generating any other response about the task. NEVER mention a skill without actually calling this tool. Do not guess skill names — only use skills listed below."
            .to_string();
        if !listing.is_empty() {
            desc.push_str("\n\nAvailable skills:\n");
            desc.push_str(&listing);
        }
        desc
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<SkillInput>()
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: SkillInput = parse_input(&input)?;
        let skill = self
            .find(&params.skill)
            .ok_or_else(|| ToolError::failed(format!("Unknown skill: {}", params.skill)))?;
        let content = match skill_path(skill, &ctx.home) {
            // Single-line return: ✦ icon + path; the model Reads it for full instructions.
            Some(path) => format!("✦ {} — read {}", skill.name, path.display()),
            // Materialization failed (e.g. cache dir not writable): fall back to full inline
            // content, the single source.
            None => format!(
                "✦ {} — built-in skill, instructions inline:\n\n{}",
                skill.name,
                expand_skill(skill, params.args.as_deref().unwrap_or(""))
            ),
        };
        Ok(ToolResult {
            content: serde_json::Value::String(content),
            is_error: false,
            diff: None,
        })
    }
}

/// Readable file path for a skill: for disk skills it's base_dir/SKILL.md;
/// bundled skills are materialized to `$XDG_CACHE_HOME/bingo/skills/{name}/SKILL.md`
/// (or `~/.cache` without XDG_CACHE_HOME); returns None if writing fails.
fn skill_path(skill: &Skill, home: &std::path::Path) -> Option<std::path::PathBuf> {
    if !skill.base_dir.as_os_str().is_empty() {
        return Some(skill.base_dir.join("SKILL.md"));
    }
    let cache = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| home.join(".cache"));
    let dir = cache.join("bingo").join("skills").join(&skill.name);
    let path = dir.join("SKILL.md");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::write(&path, &skill.content).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(name: &str) -> Skill {
        Skill {
            name: name.into(),
            description: format!("{name} description"),
            when_to_use: None,
            argument_names: vec![],
            base_dir: PathBuf::from("/tmp/skills"),
            content: "Follow the {name} procedure.".into(),
        }
    }
    fn ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("/tmp"),
            home: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                &std::env::temp_dir(),
                "test",
            )),
            hooks: crate::settings::HooksConfig::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_title, _question, _options| {
                Box::pin(async { None })
            }),
        }
    }

    #[tokio::test]
    async fn disk_skill_returns_single_line_pointer() {
        let tool = SkillTool::new(vec![skill("pdf")]);
        let result = tool
            .call(
                serde_json::json!({ "skill": "pdf", "args": "doc" }),
                &ctx(),
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert_eq!(
            text.lines().count(),
            1,
            "单行返回，不带全量正文: {text}"
        );
        assert_eq!(text, "✦ pdf — read /tmp/skills/SKILL.md");
        assert!(
            !text.contains("Follow the {name} procedure."),
            "不再展开全量正文: {text}"
        );
    }

    #[tokio::test]
    async fn bundled_skill_materializes_to_cache_dir() {
        let mut s = skill("guide");
        s.base_dir = PathBuf::new();
        let tool = SkillTool::new(vec![s]);
        let result = tool
            .call(serde_json::json!({ "skill": "guide" }), &ctx())
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert_eq!(text.lines().count(), 1, "单行返回: {text}");
        let path = std::path::Path::new(text.strip_prefix("✦ guide — read ").unwrap());
        assert!(path.ends_with("bingo/skills/guide/SKILL.md"), "{text}");
        assert!(
            std::fs::read_to_string(path).is_ok_and(|c| c.contains("Follow the {name} procedure.")),
            "物化文件内容完整: {path:?}"
        );
        // Once materialized the model can Read it: the path acts as the base_dir.
        assert!(!text.contains("instructions inline"), "{text}");
    }

    #[tokio::test]
    async fn leading_slash_is_tolerated() {
        let tool = SkillTool::new(vec![skill("commit")]);
        let result = tool
            .call(serde_json::json!({ "skill": "/commit" }), &ctx())
            .await
            .unwrap();
        assert!(result.content.as_str().unwrap().starts_with("✦ commit — read"));
    }

    #[tokio::test]
    async fn unknown_skill_is_an_error() {
        let tool = SkillTool::new(vec![skill("pdf")]);
        let err = tool
            .call(serde_json::json!({ "skill": "nope" }), &ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unknown skill: nope"));
    }

    #[test]
    fn description_lists_skills_within_budget() {
        let tool = SkillTool::new(vec![skill("pdf"), skill("commit")]);
        let desc = tool.description();
        assert!(desc.contains("Available skills:"));
        assert!(desc.contains("- pdf: pdf description"));
        assert!(desc.contains("- commit: commit description"));
        assert!(desc.contains("Do not guess skill names"));
        assert!(
            desc.contains("✦ name"),
            "描述需说明 ✦ 标记也是技能引用: {desc}"
        );
        assert!(
            desc.contains("read that file with Read"),
            "描述需说明指针 + Read 契约: {desc}"
        );

        let empty = SkillTool::new(vec![]);
        assert!(!empty.description().contains("Available skills:"));
    }
}
