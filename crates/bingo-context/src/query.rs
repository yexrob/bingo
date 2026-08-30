//! What a contributor is asked, when the test only cares where the session is
//! working.

use std::path::{Path, PathBuf};

use bingo_sdk::{
    ContextQuery, ContextUsage, HostHandle, Item, ModelCapabilities, SessionId, SessionSummary,
    TurnId, Usage,
};
use jiff::Timestamp;

pub struct Asked {
    session: SessionSummary,
    turn: TurnId,
    items: Vec<Item>,
    usage: ContextUsage,
    capabilities: ModelCapabilities,
    cwd: PathBuf,
    host: HostHandle,
}

impl Asked {
    pub fn at(cwd: &Path) -> Self {
        Self {
            session: SessionSummary {
                tools: None,
                system_extra: None,
                driver: Default::default(),
                id: SessionId::from_raw("ses_test"),
                key: None,
                title: None,
                cwd: cwd.display().to_string(),
                parent: None,
                model: None,
                provider: None,
                created_at: Timestamp::UNIX_EPOCH,
                updated_at: Timestamp::UNIX_EPOCH,
                usage: Usage::default(),
                busy: false,
            },
            turn: TurnId::from_raw("trn_test"),
            items: Vec::new(),
            usage: ContextUsage {
                used: 0,
                window: 100_000,
                trigger: 90_000,
            },
            capabilities: ModelCapabilities {
                context_window: 100_000,
                max_output: 8_000,
                images: false,
                reasoning: false,
                count_tokens: false,
                caching: false,
            },
            cwd: cwd.to_path_buf(),
            host: bingo_sdk::testing::NoHost::handle(),
        }
    }

    pub fn query(&self) -> ContextQuery<'_> {
        ContextQuery {
            session: &self.session,
            turn: &self.turn,
            round: 0,
            items: &self.items,
            usage: &self.usage,
            capabilities: &self.capabilities,
            cwd: &self.cwd,
            host: &self.host,
        }
    }
}
