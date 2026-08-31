//! `bingo provider add`: one named endpoint, asked for and written down.
//!
//! It runs before any kernel exists — providers are registered at boot, so
//! what this writes is what the *next* run reads. Two files are touched, each
//! for what it is: the instance goes to the user settings layer, and the key,
//! if one is given, to `auth.json` (0600) and never to a file a project
//! layer commits (ADR-0017 §4).

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use bingo_auth_oauth::{CredentialStore, Entry};
use bingo_sdk::{Env, ErrorCode, KernelError};
use serde_json::{Map, Value};

use crate::login::line;

/// The wire protocol a new endpoint speaks. It is the choice, not the vendor:
/// an OpenAI-compatible proxy is `openai` whoever runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    OpenAi,
    Anthropic,
}

impl Protocol {
    fn parse(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "openai" => Some(Protocol::OpenAi),
            "anthropic" => Some(Protocol::Anthropic),
            _ => None,
        }
    }

    /// The settings key that holds this protocol's endpoints.
    fn key(self) -> &'static str {
        match self {
            Protocol::OpenAi => "openai",
            Protocol::Anthropic => "anthropic",
        }
    }

    /// Where an instance that names no endpoint of its own talks.
    fn default_base_url(self) -> &'static str {
        match self {
            Protocol::OpenAi => bingo_provider_openai::DEFAULT_BASE_URL,
            Protocol::Anthropic => bingo_provider_anthropic::DEFAULT_BASE_URL,
        }
    }
}

/// The keys that hold instances; `codex` has no `apiKey`, so this command
/// does not write one, but a name taken there is taken.
const INSTANCE_KEYS: [&str; 3] = ["anthropic", "codex", "openai"];

/// Ask, then write. `registered` is what this build already answers to, so a
/// name is refused here — before anything is written — as well as at the boot
/// that would follow (ADR-0017 §2).
pub async fn add(env: &Env, registered: BTreeSet<String>) -> Result<String, KernelError> {
    let path = env.config_dir.join("settings.json");
    let mut document = read(&path)?;
    let taken = registered.union(&instances(&document)).cloned().collect();
    let name = ask_name(&taken).await?;
    let protocol = ask_protocol().await?;
    let base_url = ask_base_url(protocol).await?;
    let key = ask_key(&name).await?;
    insert(&mut document, protocol, &name, base_url)?;
    write(&path, &document)?;
    let auth = match key {
        Some(key) => Some(store(env, &name, key)?),
        None => None,
    };
    Ok(receipt(&name, protocol, &path, auth.as_deref()))
}

/// A name is what `--provider`, `/model <name>/<model>` and `/login <name>`
/// will say, so it is one word, and one nobody answers to yet.
async fn ask_name(taken: &BTreeSet<String>) -> Result<String, KernelError> {
    eprint!("Name for this provider (what `--provider` will say): ");
    let name = line().await?;
    if name.is_empty() {
        return Err(invalid("a name is what the rest of this is about"));
    }
    if name.contains('/') || name.contains(char::is_whitespace) {
        return Err(invalid(format!(
            "`{name}`: a name is one word without `/`, \
             because it is what `--provider` and `/model <name>/<model>` say"
        )));
    }
    if taken.contains(&name) {
        return Err(invalid(format!("`{name}` is already a provider's name")));
    }
    Ok(name)
}

async fn ask_protocol() -> Result<Protocol, KernelError> {
    eprint!("Which wire protocol does it speak? [openai/anthropic]: ");
    let word = line().await?;
    Protocol::parse(&word).ok_or_else(|| {
        invalid(format!(
            "`{word}` is not a wire protocol: answer `openai` for an \
             OpenAI-compatible endpoint, `anthropic` for an Anthropic-compatible one"
        ))
    })
}

async fn ask_base_url(protocol: Protocol) -> Result<Option<String>, KernelError> {
    eprint!("Base url (empty for {}): ", protocol.default_base_url());
    let url = line().await?;
    if url.is_empty() {
        return Ok(None);
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(invalid(format!("`{url}` is not an http(s) address")));
    }
    Ok(Some(url))
}

/// The key is optional and never echoed. What is typed here goes to
/// `auth.json`; the settings file gets the endpoint and nothing secret.
async fn ask_key(name: &str) -> Result<Option<String>, KernelError> {
    eprint!("API key for {name} (optional, not shown; empty to skip): ");
    let key = unechoed().await?;
    Ok(Some(key).filter(|key| !key.is_empty()))
}

/// One line with the terminal's echo off, and on again however it ends. No
/// crate here owns a terminal (ADR-0001 keeps the terminal stack in the TUI),
/// so `stty` is what turns it off; where stdin is not a terminal there is
/// nothing to hide and nothing is run.
pub(crate) async fn unechoed() -> Result<String, KernelError> {
    let echo = Echo::off();
    let typed = line().await;
    drop(echo);
    eprintln!();
    typed
}

struct Echo(bool);

impl Echo {
    fn off() -> Self {
        Echo(std::io::stdin().is_terminal() && stty("-echo"))
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        if self.0 {
            stty("echo");
        }
    }
}

fn stty(argument: &str) -> bool {
    std::process::Command::new("stty")
        .arg(argument)
        .status()
        .is_ok_and(|status| status.success())
}

/// The file as JSON, in the order it was written (`serde_json/preserve_order`).
/// A file that is not plain JSON is not this command's to rewrite: the layers
/// are read as JSONC, and a round trip would drop the comments in it.
pub(crate) fn read(path: &Path) -> Result<Map<String, Value>, KernelError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(e) => return Err(invalid(format!("{}: {e}", path.display()))),
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(invalid(format!(
            "{} holds no settings object",
            path.display()
        ))),
        Err(e) => Err(invalid(format!(
            "{} is not plain JSON ({e}); add the instance by hand — \
             a file with comments is read at startup but not rewritten here",
            path.display()
        ))),
    }
}

/// Every instance the file already names, under any key that holds them.
fn instances(document: &Map<String, Value>) -> BTreeSet<String> {
    INSTANCE_KEYS
        .iter()
        .filter_map(|key| document.get(*key)?.get("instances")?.as_object())
        .flat_map(|instances| instances.keys().cloned())
        .collect()
}

/// `<key>.instances.<name>`, leaving every neighbour where it was.
fn insert(
    document: &mut Map<String, Value>,
    protocol: Protocol,
    name: &str,
    base_url: Option<String>,
) -> Result<(), KernelError> {
    let key = protocol.key();
    let object = document
        .entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| invalid(format!("`{key}` in the settings is not an object")))?;
    let instances = object
        .entry("instances")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            invalid(format!(
                "`{key}.instances` in the settings is not an object"
            ))
        })?;
    let mut instance = Map::new();
    if let Some(base_url) = base_url {
        instance.insert("baseUrl".into(), Value::String(base_url));
    }
    instances.insert(name.to_string(), Value::Object(instance));
    Ok(())
}

/// Through a temporary file and a rename: a settings file a person wrote is
/// not something to lose half of.
pub(crate) fn write(path: &Path, document: &Map<String, Value>) -> Result<(), KernelError> {
    let directory = path
        .parent()
        .ok_or_else(|| internal(format!("{} has no directory", path.display())))?;
    std::fs::create_dir_all(directory).map_err(|e| internal(format!("{}: {e}", path.display())))?;
    let json = serde_json::to_string_pretty(document)
        .map_err(|e| internal(format!("the settings will not encode: {e}")))?;
    let temporary = directory.join("settings.json.tmp");
    std::fs::write(&temporary, format!("{json}\n"))
        .map_err(|e| internal(format!("{}: {e}", temporary.display())))?;
    std::fs::rename(&temporary, path).map_err(|e| internal(format!("{}: {e}", path.display())))
}

fn store(env: &Env, name: &str, key: String) -> Result<PathBuf, KernelError> {
    let store = CredentialStore::new(env.data_dir.clone());
    store
        .write(name, Entry::Api { key })
        .map_err(|e| internal(e.to_string()))?;
    Ok(store.path().to_path_buf())
}

/// What was written, where, and the one line that uses it.
fn receipt(name: &str, protocol: Protocol, settings: &Path, auth: Option<&Path>) -> String {
    let mut lines = vec![format!(
        "{name} is {}.instances.{name} in {}.",
        protocol.key(),
        settings.display()
    )];
    if let Some(auth) = auth {
        lines.push(format!(
            "Its key is in {}, never in the settings.",
            auth.display()
        ));
    }
    lines.push(format!("bingo --provider {name}"));
    lines.join("\n")
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

fn internal(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_protocol_is_one_of_two_words_however_it_is_typed() {
        assert_eq!(Protocol::parse(" OpenAI "), Some(Protocol::OpenAi));
        assert_eq!(Protocol::parse("anthropic"), Some(Protocol::Anthropic));
        assert_eq!(Protocol::parse("claude"), None);
        assert_eq!(Protocol::parse(""), None);
    }

    #[test]
    fn an_instance_joins_the_document_without_moving_its_neighbours() {
        let mut document: Map<String, Value> = serde_json::from_value(json!({
            "provider": "openai",
            "openai": { "apiKey": "sk-mine", "instances": { "proxy1": {} } },
        }))
        .expect("a settings object");
        insert(
            &mut document,
            Protocol::OpenAi,
            "proxy2",
            Some("http://127.0.0.1:8080".into()),
        )
        .expect("the instance is written");
        insert(&mut document, Protocol::Anthropic, "claude-proxy", None)
            .expect("the instance is written");
        assert_eq!(
            Value::Object(document.clone()),
            json!({
                "provider": "openai",
                "openai": {
                    "apiKey": "sk-mine",
                    "instances": {
                        "proxy1": {},
                        "proxy2": { "baseUrl": "http://127.0.0.1:8080" },
                    },
                },
                "anthropic": { "instances": { "claude-proxy": {} } },
            })
        );
        assert_eq!(
            instances(&document),
            ["claude-proxy", "proxy1", "proxy2"]
                .into_iter()
                .map(str::to_string)
                .collect()
        );
    }

    #[test]
    fn a_key_that_is_not_an_object_is_refused_rather_than_overwritten() {
        let mut document: Map<String, Value> =
            serde_json::from_value(json!({ "openai": "nonsense" })).expect("a settings object");
        let refused = insert(&mut document, Protocol::OpenAi, "proxy1", None)
            .expect_err("a refusal")
            .message;
        assert!(refused.contains("`openai`"), "{refused}");
    }

    #[test]
    fn a_missing_file_is_an_empty_document_and_a_broken_one_is_an_error() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("settings.json");
        assert!(read(&path).expect("no file is no settings").is_empty());
        std::fs::write(&path, "  \n").expect("a blank file");
        assert!(read(&path).expect("a blank file is no settings").is_empty());
        std::fs::write(&path, "// a comment\n{ \"openai\": {} }").expect("a jsonc file");
        let refused = read(&path).expect_err("a refusal").message;
        assert!(refused.contains("not plain JSON"), "{refused}");
    }

    #[test]
    fn a_receipt_names_both_files_and_the_command_that_uses_them() {
        assert_eq!(
            receipt(
                "proxy1",
                Protocol::OpenAi,
                Path::new("/home/me/.bingo/settings.json"),
                Some(Path::new("/home/me/.bingo/data/auth.json")),
            ),
            "proxy1 is openai.instances.proxy1 in /home/me/.bingo/settings.json.\n\
             Its key is in /home/me/.bingo/data/auth.json, never in the settings.\n\
             bingo --provider proxy1"
        );
        assert_eq!(
            receipt(
                "proxy1",
                Protocol::Anthropic,
                Path::new("/home/me/.bingo/settings.json"),
                None,
            ),
            "proxy1 is anthropic.instances.proxy1 in /home/me/.bingo/settings.json.\n\
             bingo --provider proxy1"
        );
    }
}
