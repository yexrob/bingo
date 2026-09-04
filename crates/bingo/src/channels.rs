//! `bingo channels add <adapter>` and `bingo channels secret <adapter>`: a
//! channel configured in one sitting, and a secret rotated on its own
//! (ADR-0020 §8, user-directed 2026-09-01: the add flow asks for the app id
//! and the secret together).
//!
//! Both run before any kernel exists, like `provider add` — an adapter is
//! built at boot, so what these write is what the *next* run reads. Two files
//! are touched, each for what it is: the app id goes to the user settings
//! layer, the secret to `auth.json` (0600) and never to a file a project
//! layer commits. The typing of a secret is not echoed and the value is never
//! printed back, here or by `doctor`.

use bingo_sdk::{Env, ErrorCode, KernelError};
use serde_json::Value;

use crate::login::line;
use crate::provider::unechoed;

/// Ask for everything the adapter needs — the app id in the clear (it is
/// public), the secret unechoed — and write each where it belongs.
pub async fn add(env: &Env, adapter: &str) -> Result<String, KernelError> {
    let wanted = signing(adapter)?;
    eprint!("App id for {adapter} (public, goes to the settings): ");
    let app_id = line().await?;
    if app_id.is_empty() {
        return Err(invalid("an app id is what the platform knows you as"));
    }
    eprint!("Paste the {adapter} app secret (not shown): ");
    let secret = unechoed().await?;
    if secret.is_empty() {
        return Err(invalid(
            "nothing was pasted; neither file was touched".to_string(),
        ));
    }
    let settings = configured(env, adapter, &app_id)?;
    let auth = bingo_channels::secret::store(env, wanted.id, secret)
        .map_err(|e| KernelError::new(ErrorCode::Internal, e))?;
    Ok([
        format!(
            "{adapter} is configured: its app id is in {}, its secret in {} \
             under `{}`, mode 0600.",
            settings.display(),
            auth.display(),
            bingo_channels::secret::credential(adapter)
        ),
        "`bingo gateway restart` (or `start`) picks it up.".into(),
    ]
    .join("\n"))
}

/// The app id into the user settings layer, through the same round trip
/// `provider add` uses; the spelling of the key is the plugin's
/// (`from_flags`), so this file never learns how a channel is written down.
fn configured(env: &Env, adapter: &str, app_id: &str) -> Result<std::path::PathBuf, KernelError> {
    let path = bingo_core::settings::user_path(env);
    let mut document = crate::provider::read(&path)?;
    let layer = bingo_channels::from_flags(&[format!("{adapter}={app_id}")])
        .map_err(|e| KernelError::new(ErrorCode::InvalidInput, e))?;
    merge(&mut document, layer)?;
    crate::provider::write(&path, &document)?;
    Ok(path)
}

/// The flag layer's `channels.<adapter>` object into the document, replacing
/// that adapter's entry and moving nothing beside it.
fn merge(
    document: &mut serde_json::Map<String, Value>,
    layer: serde_json::Map<String, Value>,
) -> Result<(), KernelError> {
    let Some(Value::Object(named)) = layer.get(bingo_channels::SETTING) else {
        return Ok(());
    };
    let channels = document
        .entry(bingo_channels::SETTING)
        .or_insert_with(|| Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            invalid(format!(
                "`{}` in the settings is not an object",
                bingo_channels::SETTING
            ))
        })?;
    for (name, value) in named {
        channels.insert(name.clone(), value.clone());
    }
    Ok(())
}

/// Ask for the secret, refuse an adapter that signs with nothing, and write it.
pub async fn secret(env: &Env, adapter: &str) -> Result<String, KernelError> {
    let wanted = signing(adapter)?;
    let variable = wanted.variable.unwrap_or_default();
    eprint!("Paste the {adapter} app secret (not shown): ");
    let pasted = unechoed().await?;
    if pasted.is_empty() {
        return Err(invalid(
            "nothing was pasted; the store was not touched".to_string(),
        ));
    }
    let path = bingo_channels::secret::store(env, wanted.id, pasted)
        .map_err(|e| KernelError::new(ErrorCode::Internal, e))?;
    Ok(receipt(wanted.id, variable, &path))
}

/// The adapter, if it is one that signs with anything at all.
fn signing(adapter: &str) -> Result<bingo_channels::secret::Requirement, KernelError> {
    let signing = bingo_channels::secret::signing();
    signing
        .iter()
        .find(|wanted| wanted.id == adapter)
        .copied()
        .ok_or_else(|| {
            let names: Vec<&str> = signing.iter().map(|wanted| wanted.id).collect();
            invalid(format!(
                "`{adapter}` is not a channel that signs with a secret. \
                 These are: {}",
                names.join(", ")
            ))
        })
}

fn receipt(adapter: &str, variable: &str, path: &std::path::Path) -> String {
    [
        format!(
            "The {adapter} secret is in {} under `{}`, mode 0600.",
            path.display(),
            bingo_channels::secret::credential(adapter)
        ),
        format!(
            "{variable} still wins wherever it is exported; this is what a \
             gateway started by launchd or systemd reads, since it inherits no shell."
        ),
    ]
    .join("\n")
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_adapter_that_signs_is_accepted_and_the_refusal_lists_the_ones_that_do() {
        assert_eq!(signing("feishu").expect("feishu signs").id, "feishu");
        let refused = signing("loopback").expect_err("a loopback signs with nothing");
        assert!(refused.message.contains("feishu"), "{}", refused.message);
        let refused = signing("telegram").expect_err("no such adapter");
        assert!(
            refused.message.contains("`telegram`"),
            "{}",
            refused.message
        );
    }

    #[test]
    fn the_receipt_names_the_file_and_the_key_and_no_secret() {
        let said = receipt(
            "feishu",
            "BINGO_FEISHU_APP_SECRET",
            std::path::Path::new("/home/me/.bingo/data/auth.json"),
        );
        assert!(said.contains("/home/me/.bingo/data/auth.json"), "{said}");
        assert!(said.contains("`channels.feishu`"), "{said}");
        assert!(said.contains("0600"), "{said}");
        assert!(
            said.contains("BINGO_FEISHU_APP_SECRET still wins"),
            "{said}"
        );
    }
}
