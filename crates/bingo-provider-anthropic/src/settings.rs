//! The settings key this plugin claims, as data (ADR-0017 §1).
//!
//! The key holds one endpoint and, under `instances`, any number of further
//! ones of the same type — so the default endpoint and a named one cannot
//! drift apart, and an instance cannot hold instances of its own.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

/// One Anthropic-shaped endpoint: the `anthropic` key itself, or one named
/// instance of it.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct AnthropicEndpoint {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Whether image parts reach the model. `false` for a proxy that strips
    /// them: what the *model* can see is the kernel catalogue's to say
    /// (ADR-0004), this is only what the endpoint forwards.
    pub images: bool,
}

impl Default for AnthropicEndpoint {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            images: true,
        }
    }
}

/// The `anthropic` settings key.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct AnthropicConfig {
    #[serde(flatten)]
    pub endpoint: AnthropicEndpoint,
    /// One more provider per name, registered under that name (ADR-0017 §2).
    pub instances: BTreeMap<String, AnthropicEndpoint>,
}

/// The slice the host hands `register`: the claimed key and nothing else.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    pub anthropic: AnthropicConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape ADR-0017 §1 names, read from the JSON a person writes.
    #[test]
    fn an_instance_carries_its_parents_fields_and_no_instances_of_its_own() {
        let settings: Settings = serde_json::from_value(json!({
            "anthropic": {
                "apiKey": "sk-ant-default",
                "instances": {
                    "proxy1": { "baseUrl": "http://127.0.0.1:8080", "images": false },
                    "proxy2": { "apiKey": "sk-ant-two" },
                },
            },
        }))
        .expect("the settings parse");

        assert_eq!(
            settings.anthropic.endpoint.api_key.as_deref(),
            Some("sk-ant-default")
        );
        assert!(
            settings.anthropic.endpoint.images,
            "images default to forwarded"
        );
        let proxy1 = &settings.anthropic.instances["proxy1"];
        assert_eq!(proxy1.base_url.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(proxy1.api_key, None);
        assert!(!proxy1.images);
        assert!(
            settings.anthropic.instances["proxy2"].images,
            "an instance defaults like its parent"
        );
    }

    #[test]
    fn an_absent_key_is_the_default_endpoint_and_no_instances() {
        let settings: Settings = serde_json::from_value(json!({})).expect("the settings parse");
        assert!(settings.anthropic.instances.is_empty());
        assert_eq!(settings.anthropic.endpoint.api_key, None);
        assert!(settings.anthropic.endpoint.images);
    }
}
