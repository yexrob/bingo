//! `bingo plugins list | enable | disable <name>`: the headless twin of
//! `/plugins` (ADR-0057 §6).
//!
//! It runs before a kernel exists, as `bingo mcp` does: what `enable` and
//! `disable` write is what the *next* run reads, and `list` says what the next
//! start will do — the same verdicts on the same composition, plus the plugin
//! directories this machine holds, read the way a run reads them and spawned
//! the way a listing spawns them, which is not at all.
//!
//! Stdout carries the answer and nothing else; a directory that would not read
//! is a diagnostic on stderr, as every diagnostic is.

use std::collections::BTreeSet;
use std::path::Path;

use bingo_core::plugins::{ignored, needed, state};
use bingo_core::settings::{self, Claim, Layer};
use bingo_plugin_rpc::discovery::{self, Found};
use bingo_plugin_rpc::notice::Notices;
use bingo_sdk::{Env, ErrorCode, KernelError, Plugin, PluginStatus, SWITCHED_OFF};
use bingo_surface_print::notice_report;
use clap::Subcommand;

/// What one `bingo plugins` line asks for, in clap's own shape.
#[derive(Clone, Debug, Subcommand)]
pub enum Action {
    /// Every plugin the next start will consider, and whether it will run.
    List,
    /// Turn one on in the user settings layer.
    Enable {
        /// The plugin's name, as `list` writes it.
        name: String,
    },
    /// Turn one off in the user settings layer.
    Disable {
        /// The plugin's name, as `list` writes it.
        name: String,
    },
}

/// Do it, and answer with the one thing stdout carries.
pub fn run(
    action: &Action,
    env: &Env,
    cwd: &Path,
    layers: &[Layer],
    composed: Vec<Box<dyn Plugin>>,
) -> Result<i32, KernelError> {
    let switched_off = switches(&composed, layers)?;
    let listed = listing(&composed, env, cwd, &switched_off);
    let said = match action {
        Action::List => list(&listed),
        Action::Enable { name } => switched(env, &composed, &listed, name, true)?,
        Action::Disable { name } => switched(env, &composed, &listed, name, false)?,
    };
    println!("{said}");
    Ok(0)
}

/// The names the layers turned off. The claims come from the composition, so
/// the merge is the one a run would do and a wrong type is refused here too.
fn switches(
    composed: &[Box<dyn Plugin>],
    layers: &[Layer],
) -> Result<BTreeSet<String>, KernelError> {
    let claims: Vec<Claim> = composed
        .iter()
        .filter_map(|plugin| Claim::from_manifest(plugin.manifest()))
        .collect();
    let merged = settings::merge(layers, &claims).map_err(|e| invalid(e.to_string()))?;
    Ok(merged.kernel.switched_off())
}

/// What the next start will do: the build's own plugins first, then what this
/// machine has installed under `plugins/`.
fn listing(
    composed: &[Box<dyn Plugin>],
    env: &Env,
    cwd: &Path,
    switched_off: &BTreeSet<String>,
) -> Vec<PluginStatus> {
    let mut listed = built_in(composed, switched_off);
    listed.extend(external(env, cwd, switched_off));
    listed
}

/// This build's own, under the same verdict the registry would reach: the
/// switches first, then the requirements, to a fixpoint (ADR-0057 §2).
fn built_in(composed: &[Box<dyn Plugin>], switched_off: &BTreeSet<String>) -> Vec<PluginStatus> {
    composed
        .iter()
        .zip(bingo_core::host::standing(composed, switched_off))
        .map(|(plugin, reason)| match reason {
            Some(reason) => PluginStatus::disabled(plugin.manifest(), reason),
            None => PluginStatus::loaded(plugin.manifest()),
        })
        .collect()
}

/// The plugin directories this machine holds. Nothing is spawned, so nothing
/// here knows whether a process would answer: what it says is what the switch
/// says, which is what the next start will act on.
fn external(env: &Env, cwd: &Path, switched_off: &BTreeSet<String>) -> Vec<PluginStatus> {
    let notices = Notices::default();
    let found = discovery::discover(&discovery::dirs(env, cwd), &notices);
    let human = std::io::IsTerminal::is_terminal(&std::io::stderr());
    for notice in notices.drain() {
        eprintln!("{}", notice_report(&notice.code, &notice.text, human));
    }
    found
        .into_iter()
        .map(|found| installed(found, switched_off))
        .collect()
}

fn installed(found: Found, switched_off: &BTreeSet<String>) -> PluginStatus {
    let off = switched_off.contains(&found.name);
    PluginStatus {
        id: found.name,
        version: found.manifest.version,
        enabled: !off,
        reason: off.then(|| SWITCHED_OFF.to_string()),
        from: bingo_plugin_rpc::ID.to_string(),
    }
}

/// One line per plugin, tab-separated as `bingo mcp list` is, so a shell reads
/// it as easily as a person does.
fn list(listed: &[PluginStatus]) -> String {
    listed.iter().map(row).collect::<Vec<_>>().join("\n")
}

fn row(status: &PluginStatus) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        status.id,
        status.version,
        state(status.enabled),
        status.reason.clone().unwrap_or_default()
    )
}

/// Write one switch into the user settings layer, or say why this is not one
/// to write. The refusals are `/plugins`'s own (ADR-0057 §3, §6).
fn switched(
    env: &Env,
    composed: &[Box<dyn Plugin>],
    listed: &[PluginStatus],
    name: &str,
    enabled: bool,
) -> Result<String, KernelError> {
    refuse(composed, listed, name)?;
    bingo_core::plugins::switch(env, name, enabled)
        .map_err(|e| KernelError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(format!("{name} is {} at the next start.", state(enabled)))
}

fn refuse(
    composed: &[Box<dyn Plugin>],
    listed: &[PluginStatus],
    name: &str,
) -> Result<(), KernelError> {
    if !listed.iter().any(|status| status.id == name) {
        return Err(invalid(format!(
            "no plugin named `{name}`; `bingo plugins list` lists them"
        )));
    }
    let why = composed
        .iter()
        .map(|plugin| plugin.manifest())
        .find(|manifest| manifest.id == name)
        .and_then(needed);
    match why {
        Some(why) => Err(invalid(ignored(name, why))),
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
            version: "0.1.0".into(),
            enabled,
            reason: reason.map(str::to_string),
            from: bingo_sdk::BUILT_IN.into(),
        }
    }

    /// One line each, four columns, and a standing plugin's reason is empty
    /// rather than a word standing in for one.
    #[test]
    fn a_listing_is_one_tab_separated_line_per_plugin() {
        let said = list(&[
            status("bingo.tools.web", true, None),
            status("wordcount", false, Some(SWITCHED_OFF)),
        ]);
        assert_eq!(
            said,
            format!("bingo.tools.web\t0.1.0\ton\t\nwordcount\t0.1.0\toff\t{SWITCHED_OFF}")
        );
    }

    /// The composition is what says a name exists, so a name nobody ships and
    /// nobody installed is refused before any file is touched.
    #[test]
    fn a_name_no_listing_knows_is_refused() {
        let listed = [status("bingo.tools.web", true, None)];
        let refused = refuse(&[], &listed, "bingo.tools.wb").expect_err("no such plugin");
        assert_eq!(refused.code, ErrorCode::InvalidInput);
        assert!(refused.message.contains("bingo plugins list"), "{refused}");
        refuse(&[], &listed, "bingo.tools.web").expect("a name the listing knows");
    }
}
