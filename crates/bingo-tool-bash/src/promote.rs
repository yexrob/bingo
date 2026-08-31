//! Promotion: the flag a running foreground command listens for, and the
//! command that flips it.
//!
//! Backgrounding a command thirty seconds in is a judgement a person makes,
//! not a flag guessed up front (ADR-0018 §6), so the decision has to reach a
//! call that is already in flight. Every foreground `Bash` call leaves a token
//! here under its call id while it runs; `bash.promote` flips the one it
//! names, the call's wait ends, and the same process goes to the job table.
//! The listener takes its entry away when the call is over, so a call id is
//! never promotable twice.
//!
//! The command is `instant`, because a turn is running by definition when
//! there is anything to promote, and it takes the call id as its argument —
//! which is what an `Input::Action` from a surface carries (ADR-0008 §1).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, CancellationToken, Command, CommandContext, CommandOutcome, CommandSpec, KernelError,
};

/// The name a surface fires to background the command it is watching. A
/// surface cannot import this crate (ADR-0001), so the string is the contract.
pub const ACTION: &str = "bash.promote";

/// The foreground calls that can still be taken into the background.
#[derive(Debug, Default)]
pub struct Promotions {
    open: Mutex<BTreeMap<String, CancellationToken>>,
}

impl Promotions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Listen for a promotion of `call` until the guard is dropped.
    pub fn listen(self: &Arc<Self>, call: &str) -> Listener {
        let token = CancellationToken::new();
        self.locked().insert(call.to_string(), token.clone());
        Listener {
            promotions: self.clone(),
            call: call.to_string(),
            token,
        }
    }

    /// Flip the flag for `call`; `false` when no such call is running.
    pub fn promote(&self, call: &str) -> bool {
        let Some(token) = self.locked().get(call.trim()).cloned() else {
            return false;
        };
        token.cancel();
        true
    }

    /// The calls a surface could name, for the message when it named none.
    pub fn open(&self) -> Vec<String> {
        self.locked().keys().cloned().collect()
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, CancellationToken>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One call's entry, taken away when the call ends however it ends.
#[derive(Debug)]
pub struct Listener {
    promotions: Arc<Promotions>,
    call: String,
    token: CancellationToken,
}

impl Listener {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.promotions.locked().remove(&self.call);
    }
}

/// What a surface fires, and what a person may type.
pub struct PromoteCommand {
    promotions: Arc<Promotions>,
}

impl PromoteCommand {
    pub fn new(promotions: Arc<Promotions>) -> Self {
        Self { promotions }
    }
}

#[async_trait]
impl Command for PromoteCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: ACTION.into(),
            aliases: Vec::new(),
            hint: "background the running shell command".into(),
            args: ArgSpec::Free {
                hint: "the call id".into(),
            },
            // There is only anything to promote while a turn is running.
            instant: true,
            family: "shell".into(),
        }
    }

    async fn run(&self, args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let message = match self.promotions.promote(args) {
            true => "backgrounding it".into(),
            false => nothing(&self.promotions.open(), args),
        };
        Ok(CommandOutcome::Applied {
            message: Some(message),
        })
    }
}

/// Why nothing was promoted: no command is running at all, or not that one.
fn nothing(open: &[String], asked: &str) -> String {
    match open {
        [] => "no shell command is running".into(),
        _ if asked.trim().is_empty() => "say which call to background".into(),
        _ => format!("no shell command is running as `{}`", asked.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listening_call_is_promoted_by_name() {
        let promotions = Arc::new(Promotions::new());
        let listener = promotions.listen("call_1");
        assert_eq!(promotions.open(), ["call_1"]);
        assert!(!listener.token().is_cancelled());
        assert!(promotions.promote("call_1"));
        assert!(listener.token().is_cancelled());
    }

    #[test]
    fn a_call_nobody_is_running_is_not_promoted() {
        let promotions = Arc::new(Promotions::new());
        assert!(!promotions.promote("call_1"));
        assert!(promotions.open().is_empty());
    }

    #[test]
    fn a_call_that_has_ended_can_no_longer_be_promoted() {
        let promotions = Arc::new(Promotions::new());
        drop(promotions.listen("call_1"));
        assert!(!promotions.promote("call_1"));
        assert!(promotions.open().is_empty());
    }

    #[test]
    fn two_calls_are_promoted_apart() {
        let promotions = Arc::new(Promotions::new());
        let one = promotions.listen("call_1");
        let two = promotions.listen("call_2");
        assert!(promotions.promote(" call_2 "), "a name is trimmed");
        assert!(!one.token().is_cancelled());
        assert!(two.token().is_cancelled());
    }
}
