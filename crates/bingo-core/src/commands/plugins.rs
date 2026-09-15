//! `/plugins`, alias `/modules`: which plugins are on, which are off and why,
//! and the two words that change one (ADR-0057 §6).
//!
//! Bare it is one table of what this build registered and what every source
//! knows of its own, so a person reads the built-in ones and the external ones
//! in one place. `enable` and `disable` write the user settings layer and say
//! *at the next start*: a plugin registers at boot, and a switch that pretended
//! to take a running plugin's tools away would be a lie.

use std::sync::Weak;

use async_trait::async_trait;
use bingo_sdk::*;

use crate::host::Host;
use crate::plugins::{listing, state};

pub(super) struct PluginsCommand {
    pub(super) host: Weak<Host>,
}

/// The two words it takes, in the order a surface offers them.
const ENABLE: &str = "enable";
const DISABLE: &str = "disable";
/// The catalogue the name after either word comes from (ADR-0008 §6a): the
/// listing this command draws, so a person completes what it lists.
const PLUGINS: &str = "plugins";

#[async_trait]
impl Command for PluginsCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            aliases: vec!["modules".into()],
            ..super::spec(
                "plugins",
                &format!("[{ENABLE}|{DISABLE} <name>]"),
                ArgSpec::Words {
                    values: vec![ENABLE.into(), DISABLE.into()],
                    then: Some(Box::new(ArgSpec::Catalog {
                        source: PLUGINS.into(),
                    })),
                },
                true,
            )
        }
    }

    async fn run(&self, args: &str, _cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let host = super::host(&self.host)?;
        let listed = listing(host.registry()).await;
        match parse(args)? {
            None => Ok(CommandOutcome::View {
                view: table(&listed),
            }),
            Some((name, enabled)) => switched(&host, &listed, name, enabled),
        }
    }
}

/// The line as a switch to write, or `None` for the listing.
fn parse(args: &str) -> Result<Option<(&str, bool)>, KernelError> {
    let mut words = args.split_whitespace();
    let Some(verb) = words.next() else {
        return Ok(None);
    };
    let enabled = match verb {
        ENABLE => true,
        DISABLE => false,
        other => {
            let usage = format!("usage: /plugins [{ENABLE}|{DISABLE} <name>]");
            return Err(invalid(format!("unknown argument `{other}`; {usage}")));
        }
    };
    let Some(name) = words.next() else {
        return Err(invalid(format!(
            "/plugins {verb} takes a plugin's name; /plugins lists them"
        )));
    };
    match words.next() {
        Some(extra) => Err(invalid(format!(
            "/plugins {verb} takes one name, not `{extra}` as well"
        ))),
        None => Ok(Some((name, enabled))),
    }
}

fn table(listed: &[PluginStatus]) -> View {
    View::Table {
        headers: ["plugin", "version", "state", "reason", "from"]
            .map(str::to_string)
            .to_vec(),
        rows: listed.iter().map(row).collect(),
    }
}

fn row(status: &PluginStatus) -> Vec<String> {
    vec![
        status.id.clone(),
        status.version.clone(),
        state(status.enabled).to_string(),
        status.reason.clone().unwrap_or_default(),
        status.from.clone(),
    ]
}

/// Write one switch into the user layer, or say why this is not one to write.
fn switched(
    host: &Host,
    listed: &[PluginStatus],
    name: &str,
    enabled: bool,
) -> Result<CommandOutcome, KernelError> {
    refuse(host, listed, name)?;
    crate::plugins::switch(host.env(), name, enabled)
        .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(CommandOutcome::Applied {
        message: Some(format!("{name} is {} at the next start.", state(enabled))),
    })
}

/// The two switches nobody may write: a name no listing knows — the person is
/// looking at the wrong spelling — and a plugin the binary cannot run without,
/// whose switch would be ignored (ADR-0057 §3).
fn refuse(host: &Host, listed: &[PluginStatus], name: &str) -> Result<(), KernelError> {
    if !listed.iter().any(|status| status.id == name) {
        return Err(invalid(format!(
            "no plugin named `{name}`; /plugins lists them"
        )));
    }
    match host.needed(name) {
        Some(why) => Err(invalid(crate::plugins::ignored(name, why))),
        None => Ok(()),
    }
}

fn invalid(message: String) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: &str, enabled: bool, reason: Option<&str>) -> PluginStatus {
        PluginStatus {
            id: id.into(),
            version: "1.2.3".into(),
            enabled,
            reason: reason.map(str::to_string),
            from: BUILT_IN.into(),
        }
    }

    /// ADR-0008 §6a: the verb is one of two words, and the word after it is
    /// a plugin's name — the catalogue the listing is drawn from.
    #[test]
    fn the_spec_says_both_verbs_and_the_catalogue_the_name_comes_from() {
        let spec = PluginsCommand { host: Weak::new() }.spec();
        assert_eq!(spec.name, "plugins");
        assert_eq!(spec.aliases, ["modules"]);
        let ArgSpec::Words { values, then } = spec.args else {
            panic!("a verb is one of the words");
        };
        assert_eq!(values, [ENABLE, DISABLE]);
        assert_eq!(
            then.as_deref(),
            Some(&ArgSpec::Catalog {
                source: PLUGINS.into()
            })
        );
    }

    #[test]
    fn a_bare_line_is_the_listing_and_a_verb_is_a_switch() {
        assert_eq!(parse("").expect("bare"), None);
        assert_eq!(parse("   ").expect("blank"), None);
        assert_eq!(
            parse("disable bingo.tools.web").expect("a switch"),
            Some(("bingo.tools.web", false))
        );
        assert_eq!(
            parse("  enable   wordcount  ").expect("a switch"),
            Some(("wordcount", true))
        );
    }

    #[test]
    fn a_word_it_does_not_know_and_a_missing_name_are_refused() {
        for line in ["off wordcount", "toggle", "disable", "enable a b"] {
            let refused = parse(line).expect_err("{line}");
            assert_eq!(refused.code, ErrorCode::InvalidInput, "{line}");
        }
        assert!(
            parse("disable")
                .expect_err("no name")
                .message
                .contains("/plugins lists them"),
            "a person who forgot the name is told where to look"
        );
    }

    /// The row a surface draws: the state is a word, and a plugin that is
    /// standing has an empty reason rather than a placeholder.
    #[test]
    fn a_row_carries_the_five_columns_the_headers_promise() {
        let view = table(&[
            status("bingo.tools.web", true, None),
            status("bingo.tasks", false, Some("unmet requirements: service:x")),
        ]);
        let View::Table { headers, rows } = view else {
            panic!("a table");
        };
        assert_eq!(headers, ["plugin", "version", "state", "reason", "from"]);
        assert_eq!(rows[0], ["bingo.tools.web", "1.2.3", "on", "", BUILT_IN]);
        assert_eq!(
            rows[1],
            [
                "bingo.tasks",
                "1.2.3",
                "off",
                "unmet requirements: service:x",
                BUILT_IN
            ]
        );
    }
}
