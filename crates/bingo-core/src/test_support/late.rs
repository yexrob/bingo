//! Doubles for what arrives after I/O (ADR-0009): a source of each late kind,
//! fillable after the fact, and one contributor for a source to answer with.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::*;

use super::ScriptedCompactor;

/// A tool source a test fills after the fact, so a turn can see tools
/// arrive (ADR-0009).
#[derive(Default)]
pub struct ScriptedToolSource {
    tools: Mutex<Vec<Arc<dyn Tool>>>,
}

impl ScriptedToolSource {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set(&self, tools: Vec<Arc<dyn Tool>>) {
        *self.tools.lock().unwrap() = tools;
    }
}

#[async_trait]
impl ToolSource for ScriptedToolSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.lock().unwrap().clone()
    }
}

/// A command source with a fixed table.
pub struct ScriptedCommandSource {
    commands: Vec<Arc<dyn Command>>,
}

impl ScriptedCommandSource {
    pub fn new(commands: Vec<Arc<dyn Command>>) -> Arc<Self> {
        Arc::new(Self { commands })
    }
}

#[async_trait]
impl CommandSource for ScriptedCommandSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn commands(&self, _: &std::path::Path) -> Vec<Arc<dyn Command>> {
        self.commands.clone()
    }
}

/// A contributor with one user piece to its name, so a test can watch a piece
/// land in the transcript with the origin its id earns it.
pub struct FixedContributor {
    id: String,
    placement: Placement,
    text: String,
}

/// One contributor that adds `"<id> said so"` at the start of every round.
pub fn fixed_contributor(id: &str) -> Arc<dyn ContextContributor> {
    Arc::new(FixedContributor {
        id: id.to_string(),
        placement: Placement::RoundStart,
        text: format!("{id} said so"),
    })
}

#[async_trait]
impl ContextContributor for FixedContributor {
    fn id(&self) -> &str {
        &self.id
    }
    fn placement(&self) -> Placement {
        self.placement
    }
    async fn contribute(&self, _: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        Ok(vec![ContextPiece::User {
            parts: vec![ContentPart::text(self.text.clone())],
            label: self.id.clone(),
        }])
    }
}

/// A context source a test fills after the fact, so a turn can see
/// contributors arrive (ADR-0009).
pub struct ScriptedContextSource {
    id: String,
    contributors: Mutex<Vec<Arc<dyn ContextContributor>>>,
}

impl ScriptedContextSource {
    pub fn new(id: &str) -> Arc<Self> {
        Arc::new(Self {
            id: id.to_string(),
            contributors: Mutex::new(Vec::new()),
        })
    }

    pub fn set(&self, contributors: Vec<Arc<dyn ContextContributor>>) {
        *self.contributors.lock().unwrap() = contributors;
    }
}

#[async_trait]
impl ContextSource for ScriptedContextSource {
    fn id(&self) -> &str {
        &self.id
    }
    async fn contributors(&self) -> Vec<Arc<dyn ContextContributor>> {
        self.contributors.lock().unwrap().clone()
    }
}

/// A provider source a test fills after the fact, so a model can be chosen
/// from a provider that arrived after I/O (ADR-0009, ADR-0030 §2).
pub struct ScriptedProviderSource {
    providers: Mutex<Vec<Arc<dyn Provider>>>,
}

impl ScriptedProviderSource {
    pub fn new(providers: Vec<Arc<dyn Provider>>) -> Arc<Self> {
        Arc::new(Self {
            providers: Mutex::new(providers),
        })
    }

    pub fn set(&self, providers: Vec<Arc<dyn Provider>>) {
        *self.providers.lock().unwrap() = providers;
    }
}

#[async_trait]
impl ProviderSource for ScriptedProviderSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.lock().unwrap().clone()
    }
}

/// A compactor source with a fixed answer.
pub struct ScriptedCompactorSource {
    compactors: Vec<Arc<dyn Compactor>>,
}

impl ScriptedCompactorSource {
    pub fn new(compactors: Vec<Arc<ScriptedCompactor>>) -> Arc<Self> {
        Arc::new(Self {
            compactors: compactors
                .into_iter()
                .map(|c| c as Arc<dyn Compactor>)
                .collect(),
        })
    }
}

#[async_trait]
impl CompactorSource for ScriptedCompactorSource {
    fn id(&self) -> &str {
        "scripted"
    }
    async fn compactors(&self) -> Vec<Arc<dyn Compactor>> {
        self.compactors.clone()
    }
}
