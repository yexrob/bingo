//! A fold belongs to its contributor and is projected only while it is running.
use bingo_sdk::{CommandFold, CommandSpec, ItemBody, Level};

use super::{Host, Mailbox, Registry};

pub(super) struct OwnedCommandFold {
    pub plugin: String,
    pub fold: CommandFold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Conflict,
    Invalid,
}

impl Refusal {
    fn code(self) -> &'static str {
        match self {
            Self::Conflict => "COMMAND_FOLD_CONFLICT",
            Self::Invalid => "COMMAND_FOLD_INVALID",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        [Self::Conflict, Self::Invalid]
            .into_iter()
            .find(|refusal| refusal.code() == code)
    }
}

impl Registry {
    pub(super) fn add_command_fold(&mut self, plugin: &str, fold: CommandFold) {
        self.commands.folds.push(OwnedCommandFold {
            plugin: plugin.into(),
            fold,
        });
    }

    /// Commands can be registered after their folds, including the built-ins.
    pub(super) fn validate_command_folds(&mut self) {
        let specs: Vec<_> = self
            .commands
            .entries
            .iter()
            .map(|entry| entry.spec())
            .collect();
        // Off residents keep their slot so re-enabling cannot create competing folds.
        for owned in std::mem::take(&mut self.commands.folds) {
            if let Some((refusal, why)) = refusal(&owned.fold, &self.commands.folds, &specs) {
                let text = format!(
                    "{} cannot fold into {}: {why}",
                    owned.plugin, owned.fold.into
                );
                tracing::warn!(plugin = owned.plugin, %text, "command fold refused");
                self.notices.push((refusal.code().into(), text));
            } else {
                self.commands.folds.push(owned);
            }
        }
    }

    pub(super) fn folded_into<'a>(
        &self,
        name: &str,
        aliases: impl Iterator<Item = &'a str> + Clone,
    ) -> Option<&str> {
        let off = self
            .switches
            .running
            .read()
            .unwrap_or_else(|error| error.into_inner());
        // Description links may add aliases after registration was validated.
        // Every active destination stays visible, even if an alias matches a fold.
        if self
            .commands
            .folds
            .iter()
            .any(|owned| !off.contains(&owned.plugin) && owned.fold.into == name)
        {
            return None;
        }
        self.commands
            .folds
            .iter()
            .filter(|owned| !off.contains(&owned.plugin))
            .find(|owned| {
                owned
                    .fold
                    .names
                    .iter()
                    .any(|source| source == name || aliases.clone().any(|alias| alias == source))
            })
            .map(|owned| owned.fold.into.as_str())
    }
}

fn canonical<'a>(name: &'a str, specs: &'a [CommandSpec]) -> &'a str {
    specs
        .iter()
        .find(|spec| spec.name == name || spec.aliases.iter().any(|alias| alias == name))
        .map_or(name, |spec| spec.name.as_str())
}

fn contains(fold: &CommandFold, name: &str, specs: &[CommandSpec]) -> bool {
    fold.names
        .iter()
        .any(|source| canonical(source, specs) == canonical(name, specs))
}

fn refusal(
    fold: &CommandFold,
    accepted: &[OwnedCommandFold],
    specs: &[CommandSpec],
) -> Option<(Refusal, String)> {
    if fold.into.trim().is_empty() || fold.names.iter().any(|name| name.trim().is_empty()) {
        return Some((Refusal::Invalid, "command names must not be empty".into()));
    }
    if contains(fold, &fold.into, specs) {
        return Some((Refusal::Invalid, "a destination cannot fold itself".into()));
    }
    for held in accepted {
        if let Some(name) = fold
            .names
            .iter()
            .find(|name| contains(&held.fold, name, specs))
        {
            return Some((
                Refusal::Conflict,
                format!("{} already folds {name}", held.plugin),
            ));
        }
        // A fold is one hop to a visible destination, never a hidden chain.
        if contains(&held.fold, &fold.into, specs) || contains(fold, &held.fold.into, specs) {
            return Some((
                Refusal::Invalid,
                format!(
                    "destinations must stay visible; conflicts with {}",
                    held.plugin
                ),
            ));
        }
    }
    None
}

pub(super) async fn report(host: &Host, mailbox: &Mailbox) {
    for (code, text) in &host.registry.notices {
        if Refusal::from_code(code).is_none() {
            continue;
        }
        let body = ItemBody::Notice {
            level: Level::Warn,
            code: code.clone(),
            text: text.clone(),
        };
        if let Err(error) = mailbox.record(body).await {
            tracing::debug!(%error, code, "a fold refusal reached no transcript");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_codes_keep_their_notice_spelling() {
        assert_eq!(Refusal::Conflict.code(), "COMMAND_FOLD_CONFLICT");
        assert_eq!(Refusal::Invalid.code(), "COMMAND_FOLD_INVALID");
        for refusal in [Refusal::Conflict, Refusal::Invalid] {
            assert_eq!(Refusal::from_code(refusal.code()), Some(refusal));
        }
        assert_eq!(Refusal::from_code("COMMAND_FOLD_UNKNOWN"), None);
        assert_eq!(Refusal::from_code("command_fold_invalid"), None);
    }

    #[test]
    fn refusal_kinds_preserve_alias_conflicts_and_visible_destinations() {
        let specs = [CommandSpec {
            name: "first".into(),
            aliases: vec!["alternate".into()],
            ..CommandSpec::ui("fixture")
        }];
        let accepted = [OwnedCommandFold {
            plugin: "fixture".into(),
            fold: CommandFold {
                into: "panel".into(),
                names: vec!["first".into()],
            },
        }];
        for (into, name, expected) in [
            (" ", "second", Refusal::Invalid),
            ("other", " ", Refusal::Invalid),
            ("first", "alternate", Refusal::Invalid),
            ("other", "alternate", Refusal::Conflict),
            ("alternate", "second", Refusal::Invalid),
            ("other", "panel", Refusal::Invalid),
        ] {
            let fold = CommandFold {
                into: into.into(),
                names: vec![name.into()],
            };
            assert_eq!(
                refusal(&fold, &accepted, &specs).map(|(kind, _)| kind),
                Some(expected),
                "{into}: {name}",
            );
        }
    }
}
