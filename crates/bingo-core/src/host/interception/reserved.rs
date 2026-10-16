//! The names a built-in already answers to, which no mod may take.
use super::super::registry::Registry;

/// Why `name` cannot be a mod's: a built-in plugin, one of its links, or one
/// of its `<name>:ui` handlers already answers to it. A mod under that name
/// would draw handlers the kernel resolves to the built-in's, signed as the
/// person's own press. A grant for such a name is refused for the same
/// reason: the plugin it names never loads as itself.
pub(crate) fn taken(registry: &Registry, name: &str) -> Option<String> {
    let handler = bingo_sdk::CommandSpec::ui_name(name);
    let builtin = registry.plugins.iter().any(|status| status.id == name)
        || registry
            .links
            .iter()
            .any(|link| link.plugin == name || link.interceptor.id() == name)
        || registry
            .commands
            .entries
            .iter()
            .any(|command| command.spec().name == handler);
    builtin.then(|| format!("the name {name} belongs to a built-in"))
}
