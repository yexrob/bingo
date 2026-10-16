//! Passive observers accompanying hot catalogue contributions must stop with
//! their owner, even when a session already holds its hook snapshot.
use std::{
    collections::BTreeSet,
    sync::{Arc, RwLock},
};

use bingo_sdk::{Frame, Hook, HookContext, HookMatcher, HookPoint};

use super::Registry;

pub(super) fn event_only(matcher: &HookMatcher) -> bool {
    !matcher.points.is_empty()
        && matcher
            .points
            .iter()
            .all(|point| *point == HookPoint::Event)
}

impl Registry {
    pub(super) fn add_hook(&mut self, plugin: &str, hook: Arc<dyn Hook>) {
        let hook = if event_only(&hook.matcher()) {
            Arc::new(EventHook {
                owner: plugin.into(),
                hook,
                running: self.switches.running.clone(),
            }) as Arc<dyn Hook>
        } else {
            hook
        };
        self.hooks.push(hook);
    }
}

struct EventHook {
    owner: String,
    hook: Arc<dyn Hook>,
    running: Arc<RwLock<BTreeSet<String>>>,
}

#[async_trait::async_trait]
impl Hook for EventHook {
    fn id(&self) -> &str {
        self.hook.id()
    }

    fn matcher(&self) -> HookMatcher {
        self.hook.matcher()
    }

    async fn on_event(&self, frame: &Frame, cx: &HookContext) {
        let enabled = !self
            .running
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&self.owner);
        if enabled {
            self.hook.on_event(frame, cx).await;
        }
    }
}
