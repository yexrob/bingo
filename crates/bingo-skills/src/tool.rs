//! `Skill`: the model's way in. It names a skill and gets that skill's
//! instructions back as the tool result, so the body costs context only when
//! it is wanted.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::expand::expand;
use crate::library::Library;
use crate::listing;

const DESCRIPTION: &str = "\
Load a skill: a procedure written down for this project or this machine. The \
result is the skill's own instructions, which you then follow. The skills \
available in this session, and what each is for, are listed in the system \
prompt; pass one of those names exactly. `arguments` is free text the skill \
substitutes into its instructions.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SkillArgs {
    /// The name of the skill to load, as the system prompt lists it.
    pub name: String,
    /// Text the skill substitutes into its instructions.
    pub arguments: Option<String>,
}

/// Reads a `SKILL.md` and returns it; it changes nothing.
#[derive(Debug)]
pub struct SkillTool {
    library: Arc<Library>,
}

impl SkillTool {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library }
    }

    /// The skill a call names, or a result saying what could have been named.
    fn load(&self, args: &SkillArgs, cwd: &Path) -> ToolOutput {
        let skills = self.library.skills(cwd);
        let Some(skill) = skills.iter().find(|skill| skill.name == args.name) else {
            return ToolOutput::error(unknown(&args.name, &listing::names(&skills)));
        };
        ToolOutput::text(expand(skill, args.arguments.as_deref().unwrap_or_default()))
    }
}

fn unknown(name: &str, available: &str) -> String {
    if available.is_empty() {
        return format!("no skill is called {name}, and none is available here");
    }
    format!("no skill is called {name}; the ones available are: {available}")
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Skill".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<SkillArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    fn subjects(&self, input: &Value, _cwd: &Path) -> Vec<Subject> {
        serde_json::from_value::<SkillArgs>(input.clone())
            .map(|args| vec![Subject::Name { name: args.name }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: SkillArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        Ok(self.load(&args, &cx.cwd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Tree, tool_context};
    use serde_json::json;

    async fn call(tree: &Tree, input: Value) -> ToolOutput {
        let library = Arc::new(Library::new(bingo_sdk::Env::rooted(tree.root())));
        SkillTool::new(library)
            .call(input, &tool_context(&tree.cwd()))
            .await
            .expect("a named skill is not a failure")
    }

    fn text(output: &ToolOutput) -> String {
        output
            .parts
            .iter()
            .filter_map(bingo_sdk::ContentPart::as_text)
            .collect()
    }

    #[tokio::test]
    async fn a_named_skill_comes_back_expanded_under_its_own_directory() {
        let tree = Tree::new();
        let dir = tree.user_skill("deploy", "---\ndescription: Ship\n---\nDeploy $1 now.\n");

        let output = call(&tree, json!({"name": "deploy", "arguments": "staging"})).await;
        assert!(!output.is_error);
        assert_eq!(
            text(&output),
            format!(
                "Base directory for this skill: {}\n\nDeploy staging now.\n",
                dir.display()
            ),
            "a relative path in the body has something to be relative to"
        );
    }

    #[tokio::test]
    async fn a_skill_called_without_arguments_still_comes_back() {
        let tree = Tree::new();
        tree.user_skill("deploy", "Deploy the build.\n");

        let output = call(&tree, json!({"name": "deploy"})).await;
        assert!(
            text(&output).ends_with("Deploy the build.\n"),
            "{}",
            text(&output)
        );
    }

    #[tokio::test]
    async fn a_name_nobody_has_says_which_names_exist() {
        let tree = Tree::new();
        tree.user_skill("deploy", "body\n");

        let output = call(&tree, json!({"name": "deply"})).await;
        assert!(output.is_error);
        let text = text(&output);
        assert!(text.contains("no skill is called deply"), "{text}");
        assert!(text.contains("deploy"), "{text}");
        assert!(text.contains("guide"), "{text}");
    }

    #[tokio::test]
    async fn input_the_schema_does_not_describe_is_refused() {
        let tree = Tree::new();
        let library = Arc::new(Library::new(bingo_sdk::Env::rooted(tree.root())));
        let error = SkillTool::new(library)
            .call(json!({"arguments": "x"}), &tool_context(&tree.cwd()))
            .await
            .expect_err("a call with no name names no skill");
        assert!(matches!(error, ToolError::InvalidInput(_)));
    }

    #[test]
    fn it_reads_and_a_rule_may_name_the_skill_it_reads() {
        let library = Arc::new(Library::new(bingo_sdk::Env::rooted("/tmp/nowhere")));
        let tool = SkillTool::new(library);
        assert_eq!(tool.spec().name, "Skill");
        assert!(tool.spec().meta.is_empty());
        assert_eq!(tool.traits(&Value::Null), ToolTraits::read_only());
        assert_eq!(
            tool.subjects(&json!({"name": "deploy"}), Path::new("/work")),
            [Subject::Name {
                name: "deploy".into()
            }]
        );
        assert!(tool.subjects(&Value::Null, Path::new("/work")).is_empty());
    }
}
