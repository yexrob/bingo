//! `bingo acp-mcp-proxy`: the pump an ACP agent spawns to reach the bridge.
//!
//! ADR-0036 §3 gives the agent a stdio server row — the one transport every
//! agent must speak — pointing at this mode of this binary. What it does is
//! move bytes between its own stdio and the run's rendezvous, and that is the
//! whole of it: no parsing, no MCP, no opinion about what crosses. Anything
//! this file decided would be a second place the bridge is implemented.
//!
//! The address and the token ride the environment rather than argv, because
//! argv is world-readable on every platform this ships on.

use bingo_provider_acp::bridge::{ADDRESS_VAR, Address, TOKEN_VAR, handshake, socket};
use bingo_sdk::{ErrorCode, KernelError};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// Dial, say which conversation this is, then move bytes until either end
/// stops.
pub async fn run() -> Result<i32, KernelError> {
    let address = Address::from_raw(required(ADDRESS_VAR)?);
    let token = required(TOKEN_VAR)?;
    let mut stream = socket::dial(&address).await.map_err(unreachable)?;
    handshake::write(&mut stream, &token)
        .await
        .map_err(unreachable)?;
    pump(stream).await;
    Ok(0)
}

/// Both are set by the run that spawned this, or this was not spawned by one.
fn required(name: &str) -> Result<String, KernelError> {
    std::env::var(name).map_err(|_| {
        KernelError::new(
            ErrorCode::InvalidInput,
            format!("{name} is not set: acp-mcp-proxy is spawned by bingo, not run by hand"),
        )
    })
}

fn unreachable(error: bingo_provider_acp::error::AcpError) -> KernelError {
    KernelError::new(
        ErrorCode::Internal,
        format!("the tool bridge did not answer: {error}"),
    )
}

/// stdin into the socket, the socket onto stdout.
///
/// The first end to close ends the proxy, and nothing waits for the other to
/// drain: a named pipe has no half-close, so a proxy that waited would hang
/// on Windows where it merely idles on unix. A client that closes its own
/// stdin has nothing outstanding to lose.
async fn pump<S: AsyncRead + AsyncWrite + Unpin>(stream: S) {
    let (mut incoming, mut outgoing) = tokio::io::split(stream);
    let up = async {
        let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut outgoing).await;
        let _ = outgoing.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut incoming, &mut tokio::io::stdout()).await;
    };
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proxy_nobody_spawned_says_which_word_it_is_missing() {
        // Absent for this process: nothing in the test binary sets it.
        let missing = required("BINGO_ACP_BRIDGE_NOTHING").expect_err("it is not set");
        assert_eq!(missing.code, ErrorCode::InvalidInput);
        assert!(
            missing.message.contains("BINGO_ACP_BRIDGE_NOTHING"),
            "{missing}"
        );
        assert!(missing.message.contains("not run by hand"), "{missing}");
    }

    // [`pump`] is this process's own stdin and stdout, so it is not something
    // an in-process test can drive without becoming the thing it is testing.
    // What it does is asserted where it matters: `tests/acp_bridge.rs` spawns
    // the real binary against a live bridge and speaks MCP through it.
}
