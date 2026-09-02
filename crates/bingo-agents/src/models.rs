//! What a spawn may choose from: `ListModels` reads the two catalogues the
//! kernel already keeps — the providers with their sign-in state, and the
//! models each serves with the facts the embedded snapshot holds — so a model
//! staffs an agent by looking instead of guessing an id (ADR-0026).
//!
//! Nothing here asks a provider anything: a live model list would be a
//! network call inside a read-only tool (ADR-0026 §4). The ids it reads may
//! still be an endpoint's own — the kernel fetches those in the background
//! and caches them — so each provider's line says when they are.

use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CatalogEntry, CatalogKind, Interrupt, Tool, ToolContext, ToolError, ToolOutput,
    ToolSpec, ToolTraits, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

const DESCRIPTION: &str = "\
List the model providers this build has, whether each is signed in, and the \
models it serves with their context window, output cap and whether they \
reason or read images. Call it before starting a sub-agent on a provider or \
model you are unsure of, and pass what you pick as `SpawnAgent`'s `provider` \
and `model`.";

/// The closing line: what these facts are, so a model reads an unknown id as
/// unknown rather than as unavailable, and where the ids themselves come from.
const SNAPSHOT: &str = "\
The facts above come from the models.dev snapshot embedded in this build, not \
from a live call to any provider; a model listed without them is one the \
snapshot does not carry, which says nothing about whether it works. The ids \
come from the same snapshot unless the provider's line says its endpoint was \
asked what it serves.";

/// What a provider's line says when its ids are its endpoint's own answer.
const FROM_ENDPOINT: &str = "ids from the endpoint";

/// What a provider serves when the catalogue files nothing under it.
const NOTHING: &str = "none listed";

/// What is shown for a model the snapshot does not know.
const UNKNOWN: &str = "no facts in the snapshot";

/// The arguments a listing takes, which is none. Named so the schema the
/// model reads is an object like every other tool's.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ModelsArgs {}

/// The catalogue as the model reads it: a provider per block, its models
/// indented under it, and one line saying where the facts came from.
fn listing(providers: &[CatalogEntry], models: &[CatalogEntry]) -> String {
    if providers.is_empty() {
        return "No providers are registered in this build.".to_string();
    }
    let mut lines = Vec::new();
    for provider in providers {
        let its_own: Vec<&CatalogEntry> = of(&provider.id, models).collect();
        lines.push(header(provider, &its_own));
        lines.extend(rows(&its_own));
    }
    lines.push(String::new());
    lines.push(SNAPSHOT.to_string());
    lines.join("\n")
}

/// A provider, what a person would have to do before it answers, and — when
/// the kernel's cached list is the endpoint's own — that these are the ids it
/// really serves (ADR-0026 §4).
fn header(provider: &CatalogEntry, models: &[&CatalogEntry]) -> String {
    let line = format!("{}  {}", provider.id, auth(&provider.meta));
    match models.iter().any(from_endpoint) {
        true => format!("{line}  {FROM_ENDPOINT}"),
        false => line,
    }
}

fn from_endpoint(model: &&CatalogEntry) -> bool {
    model.meta.get("source").and_then(Value::as_str) == Some("endpoint")
}

/// The sign-in state in the words the kernel filed it under; a state this
/// build cannot read is said to be unread, never assumed to be ready.
fn auth(meta: &Value) -> String {
    let status = meta.get("auth").cloned().unwrap_or(Value::Null);
    match serde_json::from_value::<AuthStatus>(status) {
        Ok(AuthStatus::Ready) => "signed in".to_string(),
        Ok(AuthStatus::NotApplicable) => "no sign-in needed".to_string(),
        Ok(AuthStatus::Missing { hint }) => format!("not signed in: {hint}"),
        Ok(AuthStatus::Expired { hint }) => format!("sign-in expired: {hint}"),
        Err(_) => "sign-in state unknown".to_string(),
    }
}

/// The catalogue's entries for one provider.
fn of<'a>(provider: &'a str, models: &'a [CatalogEntry]) -> impl Iterator<Item = &'a CatalogEntry> {
    models
        .iter()
        .filter(move |model| model.meta.get("provider").and_then(Value::as_str) == Some(provider))
}

/// The models filed under one provider, indented under it. A provider the
/// catalogue lists nothing for says so, which is not an error: an id it does
/// not carry can still be spawned on.
fn rows(models: &[&CatalogEntry]) -> Vec<String> {
    match models.is_empty() {
        true => vec![format!("  {NOTHING}")],
        false => models
            .iter()
            .map(|model| format!("  {}", row(model)))
            .collect(),
    }
}

/// One model as a row: its id, then what the snapshot knows about it.
fn row(model: &CatalogEntry) -> String {
    let mut row = vec![model.label.clone()];
    row.extend(facts(&model.meta));
    row.join("  ")
}

/// The snapshot's facts about a model, in the order they are read. A flag is
/// shown only when it is set, and a model with no facts says so.
fn facts(meta: &Value) -> Vec<String> {
    let mut facts = Vec::new();
    if let Some(context) = meta.get("context").and_then(Value::as_u64) {
        facts.push(format!("context {context}"));
    }
    if let Some(output) = meta.get("output").and_then(Value::as_u64) {
        facts.push(format!("output {output}"));
    }
    for flag in ["reasoning", "images"] {
        if meta.get(flag) == Some(&Value::Bool(true)) {
            facts.push(flag.to_string());
        }
    }
    match facts.is_empty() {
        true => vec![UNKNOWN.to_string()],
        false => facts,
    }
}

/// Reading the two catalogues; it starts nothing and calls nobody.
#[derive(Debug, Default, Clone, Copy)]
pub struct ListModelsTool;

#[async_trait]
impl Tool for ListModelsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ListModels".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<ModelsArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits(Interrupt::Cancel)
    }

    /// The arguments are ignored: a listing has none, and a model that sends
    /// an empty object, a null or a stray key still gets its answer.
    async fn call(&self, _input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let providers = catalog(cx, CatalogKind::Providers).await?;
        let models = catalog(cx, CatalogKind::Models).await?;
        Ok(ToolOutput::text(listing(&providers, &models)))
    }
}

async fn catalog(cx: &ToolContext, kind: CatalogKind) -> Result<Vec<CatalogEntry>, ToolError> {
    cx.host
        .catalog(kind)
        .await
        .map(|catalog| catalog.entries)
        .map_err(|e| ToolError::Failed(e.message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn provider(id: &str, auth: Value) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            label: id.to_string(),
            meta: json!({ "auth": auth }),
        }
    }

    fn model(provider: &str, id: &str, meta: Value) -> CatalogEntry {
        CatalogEntry {
            id: format!("{provider}/{id}"),
            label: id.to_string(),
            meta,
        }
    }

    fn line(text: &str, starts: &str) -> String {
        text.lines()
            .map(str::trim)
            .find(|line| line.starts_with(starts))
            .unwrap_or_else(|| panic!("no line for {starts} in {text}"))
            .to_string()
    }

    fn catalogues() -> (Vec<CatalogEntry>, Vec<CatalogEntry>) {
        let providers = vec![
            provider("anthropic", json!({ "kind": "ready" })),
            provider(
                "openai",
                json!({ "kind": "missing", "hint": "set OPENAI_API_KEY" }),
            ),
            provider("fake", json!({ "kind": "notApplicable" })),
        ];
        let models = vec![
            model(
                "anthropic",
                "claude-sonnet-4-5",
                json!({ "provider": "anthropic", "source": "catalogue", "context": 1_000_000,
                        "output": 64_000, "reasoning": true, "images": true }),
            ),
            model(
                "openai",
                "text-only-1",
                json!({ "provider": "openai", "source": "catalogue", "context": 400_000,
                        "output": 128_000, "reasoning": false, "images": false }),
            ),
            model(
                "fake",
                "fake-1",
                json!({ "provider": "fake", "source": "endpoint" }),
            ),
        ];
        (providers, models)
    }

    #[test]
    fn a_provider_carries_its_sign_in_state_and_its_models_facts() {
        let (providers, models) = catalogues();
        let text = listing(&providers, &models);
        assert_eq!(line(&text, "anthropic"), "anthropic  signed in");
        assert_eq!(
            line(&text, "claude-sonnet-4-5"),
            "claude-sonnet-4-5  context 1000000  output 64000  reasoning  images"
        );
        assert_eq!(
            line(&text, "openai"),
            "openai  not signed in: set OPENAI_API_KEY"
        );
        assert_eq!(
            line(&text, "text-only-1"),
            "text-only-1  context 400000  output 128000",
            "a flag that is off is not a flag"
        );
        assert!(text.ends_with(SNAPSHOT), "{text}");
    }

    /// The kernel's list for this one is the endpoint's own answer, and the
    /// line says so — while the facts are still the snapshot's, or nobody's.
    #[test]
    fn a_model_the_snapshot_does_not_carry_is_listed_without_facts() {
        let (providers, models) = catalogues();
        let text = listing(&providers, &models);
        assert_eq!(
            line(&text, "fake  "),
            format!("fake  no sign-in needed  {FROM_ENDPOINT}")
        );
        assert_eq!(line(&text, "fake-1"), format!("fake-1  {UNKNOWN}"));
        assert_eq!(
            line(&text, "anthropic"),
            "anthropic  signed in",
            "a provider still on the snapshot's list says nothing extra"
        );
    }

    #[test]
    fn a_provider_the_catalogue_files_nothing_under_says_so() {
        let providers = vec![provider("codex", json!({ "kind": "ready" }))];
        let text = listing(&providers, &[]);
        assert_eq!(line(&text, NOTHING), NOTHING);
        assert!(listing(&[], &[]).starts_with("No providers"));
    }

    #[test]
    fn a_sign_in_state_this_build_cannot_read_is_not_read_as_ready() {
        let unread = provider("odd", json!({ "kind": "somethingElse" }));
        assert_eq!(header(&unread, &[]), "odd  sign-in state unknown");
        assert_eq!(
            header(
                &CatalogEntry {
                    id: "bare".into(),
                    label: "bare".into(),
                    meta: Value::Null,
                },
                &[]
            ),
            "bare  sign-in state unknown"
        );
    }

    #[test]
    fn it_reads_and_takes_no_arguments() {
        let tool = ListModelsTool;
        let spec = tool.spec();
        assert_eq!(spec.name, "ListModels");
        assert!(spec.input_schema.get("$schema").is_none());
        assert_eq!(spec.input_schema["type"], "object");
        let traits = tool.traits(&Value::Null);
        assert!(traits.read_only && traits.trusted && !traits.concurrency_safe);
        assert_eq!(traits.interrupt, Interrupt::Cancel);
    }
}
