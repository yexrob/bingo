//! The five permission modes. A mode says what happens when no rule decides.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// Trusted read-only tools run; everything else asks.
    #[default]
    Default,
    /// Edits inside the working directories run without a prompt.
    AcceptEdits,
    /// Nothing that is not read-only runs at all.
    Plan,
    /// Everything runs except what only a person may decide.
    BypassPermissions,
    /// Nobody is there to answer, so what would have asked is denied.
    DontAsk,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "unknown permission mode: {0} (expected default|acceptEdits|plan|bypassPermissions|dontAsk)"
)]
pub struct UnknownMode(String);

impl FromStr for Mode {
    type Err = UnknownMode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(Self::Default),
            "acceptEdits" => Ok(Self::AcceptEdits),
            "plan" => Ok(Self::Plan),
            "bypassPermissions" => Ok(Self::BypassPermissions),
            "dontAsk" => Ok(Self::DontAsk),
            other => Err(UnknownMode(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Mode; 5] = [
        Mode::Default,
        Mode::AcceptEdits,
        Mode::Plan,
        Mode::BypassPermissions,
        Mode::DontAsk,
    ];

    #[test]
    fn a_mode_reads_back_from_its_own_name() {
        for mode in ALL {
            assert_eq!(mode.as_str().parse::<Mode>().ok(), Some(mode));
        }
    }

    #[test]
    fn json_and_the_command_line_spell_a_mode_the_same_way() {
        for mode in ALL {
            let json = serde_json::to_value(mode).ok();
            assert_eq!(json, Some(serde_json::Value::String(mode.to_string())));
        }
    }

    #[test]
    fn an_unknown_mode_is_an_error_that_names_the_choices() {
        let err = "yolo".parse::<Mode>().expect_err("not a mode");
        assert!(err.to_string().contains("bypassPermissions"), "{err}");
    }

    #[test]
    fn the_default_mode_is_default() {
        assert_eq!(Mode::default(), Mode::Default);
    }
}
