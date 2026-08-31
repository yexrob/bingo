//! The `channels` settings key: which platforms this bingo listens on.
//!
//! A credential never lives here. The settings' project layer is committed,
//! and an app secret in a committed file is an app secret that is gone
//! (ADR-0012's reason for `auth.json`, applied one level out): what a chat
//! app is called is settings, what it signs with is the environment.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::adapter::ChannelAdapter;
use crate::gate::Gate;
use crate::limits::{Dialect, Encoding, Limits};
use crate::loopback::{self, Loopback};

/// The top-level key this plugin owns.
pub const SETTING: &str = "channels";

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub channels: Channels,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Channels {
    /// The in-process adapter: a test's platform, and the shape of the wire.
    #[serde(default)]
    pub loopback: Option<LoopbackChannel>,
    /// How often a streaming answer is redrawn.
    #[serde(default)]
    pub coalesce: Coalesce,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopbackChannel {
    /// `host:port` to speak NDJSON with. Without one it records and says
    /// nothing, which is what an in-process test wants.
    #[serde(default)]
    pub peer: Option<String>,
    #[serde(default = "yes")]
    pub edits: bool,
    #[serde(default = "yes")]
    pub buttons: bool,
    #[serde(default = "yes")]
    pub typing: bool,
    #[serde(default = "yes")]
    pub threads: bool,
    /// What a group message must contain for the bot to be addressed.
    #[serde(default = "mention")]
    pub mention: String,
    /// How long one message may be, in characters.
    #[serde(default = "four_thousand")]
    pub max_text: usize,
    #[serde(default = "three")]
    pub max_actions: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Coalesce {
    /// New characters worth a redraw on their own.
    #[serde(default = "forty_eight")]
    pub min_chars: usize,
    /// The longest a person waits to see anything new.
    #[serde(default = "seven_hundred")]
    pub interval_ms: u64,
}

impl Default for Coalesce {
    fn default() -> Self {
        Self {
            min_chars: forty_eight(),
            interval_ms: seven_hundred(),
        }
    }
}

fn yes() -> bool {
    true
}

fn mention() -> String {
    "@bingo".into()
}

fn four_thousand() -> usize {
    4000
}

fn three() -> usize {
    3
}

fn forty_eight() -> usize {
    48
}

fn seven_hundred() -> u64 {
    700
}

impl Channels {
    /// Every adapter these settings ask for, in a fixed order.
    pub fn adapters(&self) -> Vec<Arc<dyn ChannelAdapter>> {
        let mut adapters: Vec<Arc<dyn ChannelAdapter>> = Vec::new();
        if let Some(settings) = &self.loopback {
            adapters.push(Arc::new(Loopback::new(settings.config())));
        }
        adapters
    }

    pub fn gate(&self) -> Gate {
        Gate {
            min_chars: self.coalesce.min_chars,
            interval: std::time::Duration::from_millis(self.coalesce.interval_ms),
        }
    }
}

impl LoopbackChannel {
    fn config(&self) -> loopback::Config {
        loopback::Config {
            limits: Limits {
                max_text: (self.max_text, Encoding::Chars),
                dialect: Dialect::Markdown,
                max_actions: self.max_actions,
                max_label: 40,
            },
            edits: self.edits,
            buttons: self.buttons,
            typing: self.typing,
            threads: self.threads,
            mention: self.mention.clone(),
            peer: self.peer.clone(),
        }
    }
}

/// Whether a merged settings object asks for a channel at all. The bin needs
/// the answer before a host exists, so it reads it here rather than keeping a
/// second spelling of the key.
pub fn wanted(settings: &Value) -> bool {
    serde_json::from_value::<Settings>(settings.clone())
        .map(|settings| !settings.channels.adapters().is_empty())
        .unwrap_or(false)
}

/// `--channels <adapter>[=<peer>]`, as the settings layer it stands for. The
/// spelling of the key is this plugin's, so the bin never learns it.
pub fn from_flags(flags: &[String]) -> Result<Map<String, Value>, String> {
    if flags.is_empty() {
        return Ok(Map::new());
    }
    let mut channels = Map::new();
    for flag in flags {
        let (adapter, peer) = match flag.split_once('=') {
            Some((adapter, peer)) => (adapter, Some(peer)),
            None => (flag.as_str(), None),
        };
        match adapter {
            Loopback::ID => channels.insert(adapter.into(), json!({ "peer": peer })),
            other => return Err(format!("no such channel adapter: {other}")),
        };
    }
    Ok(Map::from_iter([(SETTING.to_string(), channels.into())]))
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: Value) -> Settings {
        serde_json::from_value(value).expect("the settings parse")
    }

    #[test]
    fn nothing_configured_is_no_adapters_and_the_default_gate() {
        let settings = parse(json!({}));
        assert!(settings.channels.adapters().is_empty());
        assert_eq!(settings.channels.gate(), Gate::default());
        assert!(!wanted(&json!({})));
        assert!(!wanted(&json!({ "channels": {} })));
    }

    #[test]
    fn a_named_adapter_is_built_with_its_defaults() {
        let settings = parse(json!({ "channels": { "loopback": {} } }));
        let adapters = settings.channels.adapters();
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].id(), "loopback");
        assert!(adapters[0].edit().is_some(), "on unless turned off");
        assert!(wanted(&json!({ "channels": { "loopback": {} } })));
    }

    #[test]
    fn a_mechanism_can_be_turned_off_from_the_settings() {
        let settings = parse(json!({
            "channels": { "loopback": { "edits": false, "buttons": false } }
        }));
        let adapters = settings.channels.adapters();
        assert!(adapters[0].edit().is_none());
        assert!(adapters[0].buttons().is_none());
        assert!(adapters[0].typing().is_some());
    }

    #[test]
    fn the_gate_is_settable_because_a_test_should_not_have_to_wait() {
        let settings = parse(json!({
            "channels": { "coalesce": { "minChars": 1, "intervalMs": 5 } }
        }));
        assert_eq!(
            settings.channels.gate(),
            Gate {
                min_chars: 1,
                interval: std::time::Duration::from_millis(5),
            }
        );
    }

    #[test]
    fn a_flag_becomes_the_layer_the_plugin_would_have_read_from_a_file() {
        assert_eq!(
            Value::Object(from_flags(&["loopback=127.0.0.1:9".into()]).expect("a layer")),
            json!({ "channels": { "loopback": { "peer": "127.0.0.1:9" } } })
        );
        assert_eq!(
            Value::Object(from_flags(&["loopback".into()]).expect("a layer")),
            json!({ "channels": { "loopback": { "peer": null } } })
        );
        assert!(from_flags(&[]).expect("no flags").is_empty());
        assert_eq!(
            from_flags(&["telegram".into()]),
            Err("no such channel adapter: telegram".into())
        );
    }
}
