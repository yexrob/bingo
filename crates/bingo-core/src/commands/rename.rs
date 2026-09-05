//! `/rename <title>`: what this session is called, in every list that names
//! it. The name goes onto the summary, which is journalled, so a resume
//! brings it back — the same road a `/model` takes (ADR-0008 §4).

use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::Host;

const USAGE: &str = "usage: /rename <title>";
/// A name has to sit in a one-line row beside everything else a session is.
const MAX: usize = 80;

pub(super) struct RenameCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for RenameCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "rename",
            "<title>",
            ArgSpec::Free {
                hint: "a name for this session".into(),
            },
            true,
        )
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        match parse(args)? {
            None => report(&host, cx).await,
            Some(title) => set(&host, cx, title).await,
        }
    }
}

/// Bare `/rename`: the name as it stands, and how to say another.
async fn report(host: &Arc<Host>, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
    let summary = host.session_summary(&cx.session).await?;
    Ok(CommandOutcome::View {
        view: View::Text {
            text: format!("{}\n{USAGE}", named(summary.title.as_deref())),
        },
    })
}

/// `/rename <title>`: this and every list that names the session.
async fn set(
    host: &Arc<Host>,
    cx: &CommandContext,
    title: String,
) -> Result<CommandOutcome, KernelError> {
    host.reconfigure(&cx.session, SessionChange::Title(title.clone()))
        .await?;
    Ok(CommandOutcome::Applied {
        message: Some(named(Some(&title))),
    })
}

/// A title is one trimmed line, short enough for a row. `None` is no
/// argument at all, which is a question rather than a rename.
fn parse(args: &str) -> Result<Option<String>, KernelError> {
    let title = args.trim();
    if title.is_empty() {
        return Ok(None);
    }
    let length = title.chars().count();
    if length > MAX {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("a name is at most {MAX} characters, and that one is {length}; {USAGE}"),
        ));
    }
    Ok(Some(title.to_string()))
}

/// A session with no name of its own has not been asked anything yet: the
/// first ask mints one (ADR-0005 §5), so this says what will happen rather
/// than that there is nothing.
fn named(title: Option<&str>) -> String {
    match title {
        Some(title) => format!("name: {title}"),
        None => "name: none yet — the first thing you ask mints one".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_one_trimmed_line_and_no_argument_is_a_question() {
        assert_eq!(
            parse("  the release  ").unwrap(),
            Some("the release".into())
        );
        assert_eq!(parse("   ").unwrap(), None);
        assert_eq!(parse("").unwrap(), None);

        let long = "é".repeat(MAX + 1);
        let refused = parse(&long).unwrap_err();
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(refused.message.contains("81"), "{}", refused.message);
        assert_eq!(parse(&"é".repeat(MAX)).unwrap(), Some("é".repeat(MAX)));
    }

    #[test]
    fn a_session_with_no_name_is_told_where_one_comes_from() {
        assert_eq!(named(Some("the release")), "name: the release");
        assert!(named(None).starts_with("name: none yet"));
    }
}
