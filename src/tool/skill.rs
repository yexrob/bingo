use async_trait::async_trait;
use serde::Deserialize;

use crate::skills::{expand_skill, format_listing, Skill, DEFAULT_CHAR_BUDGET};

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// Skill 工具输入。
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SkillInput {
    #[schemars(description = "The skill name (e.g. \"commit\", \"review-pr\")")]
    pub skill: String,
    #[serde(default)]
    #[schemars(description = "Optional arguments for the skill")]
    pub args: Option<String>,
}

/// Skill：在技能注册表中按名执行。
/// 磁盘技能返回小展示（`Launching skill: {name}` + SKILL.md 路径），
/// 模型需要完整指令时自行 Read 文件；内置技能（无文件基准）才展开全量
/// 注入——那是它唯一的来源。
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

When users reference a slash command or \"/<something>\" (e.g. \"/commit\"), they are referring to a skill. Use this tool to invoke it.

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
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: SkillInput = parse_input(&input)?;
        let skill = self
            .find(&params.skill)
            .ok_or_else(|| ToolError::failed(format!("Unknown skill: {}", params.skill)))?;
        let content = if skill.base_dir.as_os_str().is_empty() {
            // 内置技能：无 SKILL.md 文件可读，只能展开注入。
            let expanded = expand_skill(skill, params.args.as_deref().unwrap_or(""));
            format!("Launching skill: {}\n\n{expanded}", skill.name)
        } else {
            // 磁盘技能：小展示，让模型自己 Read SKILL.md 拿完整指令。
            format!(
                "Launching skill: {}\n\nRead the full skill instructions at {}",
                skill.name,
                skill.base_dir.join("SKILL.md").display()
            )
        };
        Ok(ToolResult {
            content: serde_json::Value::String(content),
            is_error: false,
            diff: None,
        })
    }
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
    async fn disk_skill_returns_pointer_not_full_content() {
        let tool = SkillTool::new(vec![skill("pdf")]);
        let result = tool
            .call(
                serde_json::json!({ "skill": "pdf", "args": "doc" }),
                &ctx(),
            )
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.starts_with("Launching skill: pdf\n\n"));
        assert!(text.contains("/tmp/skills/SKILL.md"), "提示自行读取: {text}");
        assert!(
            !text.contains("Follow the {name} procedure."),
            "不再展开全量正文: {text}"
        );
    }

    #[tokio::test]
    async fn bundled_skill_still_expands_full_content() {
        let mut s = skill("guide");
        s.base_dir = PathBuf::new();
        let tool = SkillTool::new(vec![s]);
        let result = tool
            .call(serde_json::json!({ "skill": "guide" }), &ctx())
            .await
            .unwrap();
        let text = result.content.as_str().unwrap();
        assert!(text.starts_with("Launching skill: guide\n\n"));
        assert!(text.contains("Follow the {name} procedure."), "内置全量: {text}");
    }

    #[tokio::test]
    async fn leading_slash_is_tolerated() {
        let tool = SkillTool::new(vec![skill("commit")]);
        let result = tool
            .call(serde_json::json!({ "skill": "/commit" }), &ctx())
            .await
            .unwrap();
        assert!(result.content.as_str().unwrap().starts_with("Launching skill: commit"));
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

        let empty = SkillTool::new(vec![]);
        assert!(!empty.description().contains("Available skills:"));
    }
}
