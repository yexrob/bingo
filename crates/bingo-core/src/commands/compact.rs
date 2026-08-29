//! `/compact [instructions]`: a turn that only makes room. Not instant: it
//! waits in the queue behind a running turn, so its cut lands after that
//! turn's items.

use std::sync::Weak;

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::Host;

pub(super) struct CompactCommand {
    pub(super) host: Weak<Host>,
}

#[async_trait]
impl Command for CompactCommand {
    fn spec(&self) -> CommandSpec {
        super::spec(
            "compact",
            "[instructions]",
            ArgSpec::Free {
                hint: "what the summary should keep".into(),
            },
            false,
        )
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        let instructions = Some(args.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        host.compact(&cx.session, instructions).await?;
        Ok(CommandOutcome::Applied {
            message: Some("compacting the conversation".into()),
        })
    }
}
