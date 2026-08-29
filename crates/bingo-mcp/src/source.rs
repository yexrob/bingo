//! The contribution the kernel reads when a turn starts (ADR-0009 §1).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolSource};

use crate::manager::Manager;

/// Every connected server's tools, as they stand when asked. Answering with
/// nothing is never wrong: a server that has not landed yet is in the next
/// turn's set, not this one's.
pub struct McpSource {
    manager: Arc<Manager>,
}

impl McpSource {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSource for McpSource {
    fn id(&self) -> &str {
        "mcp"
    }

    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.manager.tools().await
    }
}
