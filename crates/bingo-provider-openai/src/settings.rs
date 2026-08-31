//! The two settings keys this plugin claims, as data (ADR-0017 §1).
//!
//! Each key holds one endpoint and, under `instances`, any number of further
//! ones of the same type — so the default endpoint and a named one cannot
//! drift apart, and an instance cannot hold instances of its own.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;

/// One OpenAI-shaped endpoint: the `openai` key itself, or one named
/// instance of it.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenAiEndpoint {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Whether image parts reach the model. `false` for a proxy that strips
    /// them: what the *model* can see is the kernel catalogue's to say
    /// (ADR-0004), this is only what the endpoint forwards.
    pub images: bool,
}

impl Default for OpenAiEndpoint {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            images: true,
        }
    }
}

/// The `openai` settings key.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenAiConfig {
    #[serde(flatten)]
    pub endpoint: OpenAiEndpoint,
    /// One more provider per name, registered under that name (ADR-0017 §2).
    pub instances: BTreeMap<String, OpenAiEndpoint>,
}

/// One ChatGPT subscription endpoint. No key and no token: a subscription
/// credential only ever comes from a login, and both fields exist for a proxy
/// or a test.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexEndpoint {
    pub base_url: Option<String>,
    pub issuer: Option<String>,
}

/// The `codex` settings key.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexConfig {
    #[serde(flatten)]
    pub endpoint: CodexEndpoint,
    /// One subscription per name, each with its own `auth.json` entry
    /// (ADR-0017 §3).
    pub instances: BTreeMap<String, CodexEndpoint>,
}

/// The slice the host hands `register`: the claimed keys and nothing else.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    pub openai: OpenAiConfig,
    pub codex: CodexConfig,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape ADR-0017 §1 names, read from the JSON a person writes.
    #[test]
    fn an_instance_carries_its_parents_fields_and_no_instances_of_its_own() {
        let settings: Settings = serde_json::from_value(json!({
            "openai": {
                "apiKey": "sk-default",
                "baseUrl": "https://api.openai.com",
                "instances": {
                    "proxy1": { "baseUrl": "http://127.0.0.1:8080", "images": false },
                    "proxy2": { "apiKey": "sk-two" },
                },
            },
            "codex": {
                "issuer": "https://auth.openai.com",
                "instances": { "work": { "baseUrl": "http://127.0.0.1:9090" } },
            },
        }))
        .expect("the settings parse");

        assert_eq!(
            settings.openai.endpoint.api_key.as_deref(),
            Some("sk-default")
        );
        assert!(
            settings.openai.endpoint.images,
            "images default to forwarded"
        );
        let proxy1 = &settings.openai.instances["proxy1"];
        assert_eq!(proxy1.base_url.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(proxy1.api_key, None);
        assert!(!proxy1.images);
        assert!(
            settings.openai.instances["proxy2"].images,
            "an instance defaults like its parent"
        );
        assert_eq!(
            settings.codex.endpoint.issuer.as_deref(),
            Some("https://auth.openai.com")
        );
        assert_eq!(
            settings.codex.instances["work"].base_url.as_deref(),
            Some("http://127.0.0.1:9090")
        );
    }

    #[test]
    fn an_absent_key_is_the_default_endpoint_and_no_instances() {
        let settings: Settings = serde_json::from_value(json!({})).expect("the settings parse");
        assert!(settings.openai.instances.is_empty());
        assert!(settings.codex.instances.is_empty());
        assert_eq!(settings.openai.endpoint.api_key, None);
        assert!(settings.openai.endpoint.images);
    }
}
