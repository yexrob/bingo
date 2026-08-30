//! The system prompt the kernel gives every session: who the model is and
//! how to behave (cacheable, identical across sessions), then an `<env>`
//! block for this session. Plugins add their own blocks through
//! `ContextContributor`; nothing here knows any of them.

use std::path::Path;

use bingo_sdk::SystemBlock;

#[derive(Clone, Copy, Debug)]
pub struct PromptInput<'a> {
    pub cwd: &'a Path,
    pub provider: &'a str,
    pub model: &'a str,
    pub platform: &'a str,
    pub date: jiff::civil::Date,
}

/// The identity block first, so the cache prefix is the same for every
/// session; the per-session `<env>` block after it.
pub fn system_blocks(input: &PromptInput<'_>) -> Vec<SystemBlock> {
    vec![
        SystemBlock {
            text: IDENTITY.trim_end().to_string(),
            cache: true,
        },
        SystemBlock {
            text: env_block(input),
            cache: false,
        },
    ]
}

fn env_block(input: &PromptInput<'_>) -> String {
    let mut out = String::from("<env>\n");
    out.push_str(&format!("Working directory: {}\n", input.cwd.display()));
    out.push_str(&format!("Platform: {}\n", input.platform));
    out.push_str(&format!("Today's date: {}\n", input.date));
    out.push_str(&format!("Model: {} ({})\n", input.model, input.provider));
    out.push_str("</env>");
    out
}

const IDENTITY: &str = "\
You are bingo, an interactive coding agent running in the user's terminal. \
Help with software engineering tasks — understanding, changing, testing and \
explaining code — using the tools available to you.

# Tone and style
- Be concise and direct. Answer in plain prose; use Markdown only where it \
helps (code, lists, tables).
- Do not narrate what you are about to do or summarise what you did unless \
asked; the user sees your tool calls.
- When you are unsure, say so and say what would settle it.

# Doing tasks
- Read before you edit: look at the code around a change before changing it, \
and follow the conventions you find there.
- Prefer editing existing files over creating new ones. Do not create \
documentation or explanation files unless asked.
- Make the change the user asked for, no more; mention adjacent problems \
rather than fixing them unasked.
- After a change, run the relevant tests or checks when you can and report \
what they said, including failures.
- Never commit, push, delete or overwrite work unless the user asked for that.

# Tool use
- Each tool says what it is for; use the one made for the job, not a shell \
command that imitates it.
- Call independent tools in the same turn so they run together.
- Paths are relative to the working directory unless absolute; quote paths \
with spaces in shell commands.
- A tool result marked as an error tells you why; adjust rather than retry \
the same call.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn input(cwd: &Path) -> PromptInput<'_> {
        PromptInput {
            cwd,
            provider: "anthropic",
            model: "claude-sonnet-4-5",
            platform: "macos",
            date: jiff::civil::date(2026, 8, 29),
        }
    }

    #[test]
    fn identity_is_cacheable_and_the_env_block_is_not() {
        let blocks = system_blocks(&input(Path::new("/work/app")));
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].cache);
        assert!(blocks[0].text.starts_with("You are bingo"));
        assert!(!blocks[1].cache);
        insta::assert_snapshot!(blocks[1].text);
    }

    #[test]
    fn the_env_block_names_the_working_directory() {
        let text = env_block(&input(Path::new("/work")));
        assert!(text.contains("Working directory: /work"));
    }
}
