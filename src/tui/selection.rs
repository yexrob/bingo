//! What the console reads about the session's configuration, and how it says
//! it.
//!
//! There is one authority for the model, the provider, the thinking level and
//! the permission mode: the core's own configuration (D154). A key that changes
//! one applies an action to the core; the core publishes what it holds; the
//! console reads it back here, and the engine mirrors it into the runtime a run
//! reads. Nothing on this side keeps a copy, because a copy is a second answer
//! and two answers can disagree — which is exactly what shift+tab and
//! `/permission-mode` used to do.

use crate::permission::PermissionMode;

/// The next permission mode in the shift+tab ladder (CC `app:cycleMode`).
///
/// `startup` is the mode the process was launched in, and it is what makes the
/// dangerous modes reachable without being *introduced*: a session that never
/// started in bypass/dontAsk can never cycle into one.
///
/// Pure since D105, because the zoom cycles a *different* subject's mode — the
/// viewed agent's — and CC does exactly that, calling its own
/// `getNextPermissionMode` on the teammate's context and leaving the leader's
/// alone (`PromptInput.tsx:1410-1447`).
/// The permission mode as the core spells it. The two vocabularies are the same
/// five names; the crossing lives here so nothing else has to know both.
pub fn app_permission_mode(mode: PermissionMode) -> crate::app::snapshot::PermissionMode {
    use crate::app::snapshot::PermissionMode as App;
    match mode {
        PermissionMode::Default => App::Default,
        PermissionMode::AcceptEdits => App::AcceptEdits,
        PermissionMode::BypassPermissions => App::BypassPermissions,
        PermissionMode::DontAsk => App::DontAsk,
        PermissionMode::Plan => App::Plan,
    }
}

/// And back, for a console reading the mode the core holds.
pub fn console_permission_mode(mode: crate::app::snapshot::PermissionMode) -> PermissionMode {
    use crate::app::snapshot::PermissionMode as App;
    match mode {
        App::Default => PermissionMode::Default,
        App::AcceptEdits => PermissionMode::AcceptEdits,
        App::BypassPermissions => PermissionMode::BypassPermissions,
        App::DontAsk => PermissionMode::DontAsk,
        App::Plan => PermissionMode::Plan,
    }
}

pub fn next_permission_mode(mode: PermissionMode, startup: PermissionMode) -> PermissionMode {
    let next = match mode {
        PermissionMode::Default => PermissionMode::AcceptEdits,
        PermissionMode::AcceptEdits => PermissionMode::Plan,
        PermissionMode::Plan => PermissionMode::Default,
        // Started in bypass/dontAsk: toggle between it and default, never introducing a new dangerous mode.
        PermissionMode::BypassPermissions | PermissionMode::DontAsk => PermissionMode::Default,
    };
    // From default, switch back to the startup mode (an edge that only bypass/dontAsk sessions have).
    if next == PermissionMode::AcceptEdits
        && matches!(
            startup,
            PermissionMode::BypassPermissions | PermissionMode::DontAsk
        )
    {
        return startup;
    }
    next
}

impl super::Chat {
    /// The model in effect, from the one place that holds it.
    ///
    /// The core's configuration is the session's selection since D154: an
    /// `action/execute`, a slash line and a key all change it there, and the
    /// engine mirrors it into the runtime a run reads. The console reads the
    /// projection the core published, and the core's own copy until the first
    /// cut lands — never the runtime, which is the engine's end of the mirror.
    pub(crate) fn model(&self) -> String {
        match &self.store.view().config {
            Some(config) => config.model.clone(),
            None => self.session.core.config().borrow().model.clone(),
        }
    }

    /// The provider in effect, from the same place.
    pub(crate) fn provider(&self) -> String {
        match &self.store.view().config {
            Some(config) => config.provider.clone(),
            None => self.session.core.config().borrow().provider.clone(),
        }
    }

    /// The thinking level in effect. `None` is `off`, which is what the wire
    /// gate reads as "send no thinking parameter".
    pub(crate) fn thinking(&self) -> Option<String> {
        let level = match &self.store.view().config {
            Some(config) => config.thinking,
            None => self.session.core.config().borrow().thinking,
        };
        match level {
            crate::app::snapshot::ThinkingLevel::Off => None,
            level => Some(level.as_str().to_string()),
        }
    }

    /// The permission mode in effect, read from the projection rather than kept.
    ///
    /// Shift+tab used to set a copy here while `/permission-mode` set the core's,
    /// which is two answers to one question. There is one now: the core's, as
    /// `config/read` publishes it. Before the first cut lands, the mode the
    /// session started in is the honest answer.
    pub fn permission_mode(&self) -> PermissionMode {
        match &self.store.view().config {
            Some(config) => console_permission_mode(config.permission_mode),
            None => self.session.permission_mode,
        }
    }
}
