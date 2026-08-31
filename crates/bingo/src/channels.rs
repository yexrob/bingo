//! `bingo channels secret <adapter>`: one secret, pasted and put where a
//! gateway started at boot can read it (ADR-0020 §8).
//!
//! It runs before any kernel exists, like `provider add` — an adapter is built
//! at boot, so what this writes is what the *next* run signs with. The typing
//! is not echoed and the value is never printed back, here or by `doctor`; the
//! receipt names the file and the key and stops there.

use bingo_sdk::{Env, ErrorCode, KernelError};

use crate::provider::unechoed;

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
