//! `bingo login|logout <provider>`: the two kernel commands run without a
//! session, answered on the terminal (ADR-0012 §5). Stdout carries the
//! receipt and nothing else; the address, the code and the waiting are on
//! stderr, as every diagnostic is.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_core::Host;
use bingo_sdk::{
    Answer, AnswerSpec, ErrorCode, InteractionKind, KernelError, LoginFlow, LoginMethod, Prompter,
};
use tokio::io::AsyncBufReadExt;

pub async fn login(
    host: &Host,
    provider: &str,
    method: Option<LoginMethod>,
) -> Result<String, KernelError> {
    let provider = host.provider(Some(provider))?;
    provider
        .login(Arc::new(Terminal), method)
        .await
        .map_err(|e| KernelError::new(e.code(), e.to_string()))
}

pub async fn logout(host: &Host, provider: &str) -> Result<String, KernelError> {
    let provider = host.provider(Some(provider))?;
    provider
        .logout()
        .await
        .map_err(|e| KernelError::new(e.code(), e.to_string()))
}

/// A person at a terminal: told on stderr, heard on stdin. A browser or
/// device flow completes on its own, so the ask waits until the flow drops
/// it; ctrl-c ends the process and the flow with it.
struct Terminal;

#[async_trait]
impl Prompter for Terminal {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        let InteractionKind::Login { provider, flow } = kind else {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "only a sign-in can be answered here",
            ));
        };
        match flow {
            LoginFlow::Browser { url } => {
                eprintln!("Sign in to {provider} in your browser. If it did not open:\n  {url}");
                eprintln!("Waiting for the browser…");
                std::future::pending().await
            }
            LoginFlow::Device { url, code } => {
                eprintln!("Sign in to {provider}: open\n  {url}\nand enter the code {code}");
                eprintln!("Waiting…");
                std::future::pending().await
            }
            LoginFlow::Paste if answers.contains(&AnswerSpec::Text) => {
                eprint!("Paste the {provider} credential: ");
                Ok(Answer::Text {
                    text: line().await?,
                })
            }
            LoginFlow::Paste => Err(KernelError::new(
                ErrorCode::InvalidInput,
                "this sign-in takes no pasted credential",
            )),
        }
    }
}

async fn line() -> Result<String, KernelError> {
    let mut text = String::new();
    tokio::io::BufReader::new(tokio::io::stdin())
        .read_line(&mut text)
        .await
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("stdin: {e}")))?;
    Ok(text.trim().to_string())
}
