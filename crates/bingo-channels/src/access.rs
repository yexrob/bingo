//! Who may speak to this bot, and where (ADR-0051 §4).
//!
//! A pure policy, per adapter, read by the surface where `engaged` used to
//! decide on a mention alone. The default is what ran before there was a
//! policy at all — open, a group engaging on a mention — so nobody's bot goes
//! quiet on upgrade.
//!
//! Nothing here does I/O or knows a platform: it is handed a conversation, the
//! principal the adapter stamped, and whether the adapter saw the bot
//! addressed, and it answers yes or names why not.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Deserialize;

use crate::conversation::Conversation;

/// One adapter's whole policy.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Access {
    /// Principals who pass every rule but `off`.
    #[serde(default)]
    pub admins: BTreeSet<String>,
    /// Direct chats. Its `mention` is meaningless: a direct message is
    /// always addressed to whoever it was sent to.
    #[serde(default)]
    pub direct: Rule,
    /// Groups with no rule of their own.
    #[serde(default)]
    pub group: Rule,
    /// One chat's rule, which replaces `group` for that chat entirely.
    #[serde(default)]
    pub rules: BTreeMap<String, Rule>,
}

/// What one kind of chat admits.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    #[serde(default)]
    pub policy: Policy,
    /// Who `allowlist` lets in, or `blocklist` keeps out. Read by neither
    /// other policy.
    #[serde(default)]
    pub list: BTreeSet<String>,
    /// Whether the bot has to be spoken to. `false` engages a group on every
    /// message, amending ADR-0016 §4.
    #[serde(default = "yes")]
    pub mention: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            policy: Policy::Open,
            list: BTreeSet::new(),
            mention: true,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    /// Anyone here.
    #[default]
    Open,
    /// Only the `list`.
    Allowlist,
    /// Anyone but the `list`.
    Blocklist,
    /// Only an admin.
    Admins,
    /// Nobody, admins included: a chat turned off is off.
    Off,
}

/// Why an arrival was not this surface's business. Named rather than a bare
/// `false`, because the one line in the log is all anybody gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("the bot was not addressed")]
    NotAddressed,
    #[error("not on the allowlist")]
    NotListed,
    #[error("on the blocklist")]
    Listed,
    #[error("not an admin")]
    NotAdmin,
    #[error("this chat is off")]
    Off,
}

impl Access {
    /// Whether this principal may be heard here.
    pub fn admits(
        &self,
        to: &Conversation,
        principal: &str,
        addressed: bool,
    ) -> Result<(), Refused> {
        let rule = self.rule(to);
        rule.admits(principal, self.admins.contains(principal))?;
        // Only a group has anything to mention: a direct message is addressed
        // by having been sent at all.
        match !to.group || !rule.mention || addressed {
            true => Ok(()),
            false => Err(Refused::NotAddressed),
        }
    }

    /// The rule this chat runs under: its own if it has one, else the one for
    /// its kind.
    fn rule(&self, to: &Conversation) -> &Rule {
        match to.group {
            false => &self.direct,
            true => self.rules.get(&to.chat).unwrap_or(&self.group),
        }
    }
}

impl Rule {
    /// The policy alone, before the mention is considered. An admin passes
    /// everything but `off`, which is off for everyone.
    fn admits(&self, principal: &str, admin: bool) -> Result<(), Refused> {
        match self.policy {
            Policy::Off => Err(Refused::Off),
            Policy::Admins => admin.then_some(()).ok_or(Refused::NotAdmin),
            _ if admin => Ok(()),
            Policy::Open => Ok(()),
            Policy::Allowlist => self
                .lists(principal)
                .then_some(())
                .ok_or(Refused::NotListed),
            Policy::Blocklist => (!self.lists(principal))
                .then_some(())
                .ok_or(Refused::Listed),
        }
    }

    fn lists(&self, principal: &str) -> bool {
        self.list.contains(principal)
    }
}

#[cfg(test)]
mod tests;
