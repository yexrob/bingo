//! The kernel's own commands (ADR-0008 §4, ADR-0012 §5): what changes the
//! next turn's model or thinking, what makes room, and a provider's
//! credential. They are registered like any plugin's and listed in the same
//! catalogue.

mod compact;
mod login;
mod model;
mod status;
mod think;

use std::sync::{Arc, Weak};

use bingo_sdk::*;

use crate::host::Host;

pub(crate) fn builtins(host: Weak<Host>) -> Vec<Arc<dyn Command>> {
    vec![
        Arc::new(model::ModelCommand { host: host.clone() }),
        Arc::new(think::ThinkCommand { host: host.clone() }),
        Arc::new(compact::CompactCommand { host: host.clone() }),
        Arc::new(login::LoginCommand { host: host.clone() }),
        Arc::new(login::LogoutCommand { host: host.clone() }),
        Arc::new(status::StatusCommand { host }),
    ]
}

fn host(weak: &Weak<Host>) -> Result<Arc<Host>, KernelError> {
    weak.upgrade()
        .ok_or_else(|| KernelError::new(ErrorCode::SessionClosed, "the host is shut down"))
}

fn spec(name: &str, hint: &str, args: ArgSpec, instant: bool) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        aliases: Vec::new(),
        hint: hint.into(),
        args,
        instant,
        family: "session".into(),
    }
}
