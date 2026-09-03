//! The first line on an accepted stream: the token, and nothing else.
//!
//! Both halves are written here so the two spell the same thing once: the
//! proxy in the binary [`write`]s it, the listener [`read`]s it. Everything
//! after the newline is MCP and belongs to `rmcp`, which is why this reads a
//! byte at a time rather than through a buffered reader — a `BufReader` would
//! swallow the start of the conversation it is handing over.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::AcpError;

/// The longest a token line may be. A token is 43 characters; anything near
/// this is not one, and a peer that sends no newline at all must not be able
/// to make this side buffer for ever.
const LIMIT: usize = 128;

/// Announce which conversation this stream is. Called by the proxy, before
/// any byte of MCP.
pub async fn write<W: AsyncWrite + Unpin>(stream: &mut W, token: &str) -> Result<(), AcpError> {
    stream
        .write_all(format!("{token}\n").as_bytes())
        .await
        .map_err(AcpError::transport)?;
    stream.flush().await.map_err(AcpError::transport)
}

/// Read the token the far side announced, and not one byte more.
pub async fn read<R: AsyncRead + Unpin>(stream: &mut R) -> Result<String, AcpError> {
    let mut offered = Vec::with_capacity(LIMIT);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte).await.map_err(AcpError::transport)? {
            0 => {
                return Err(AcpError::transport(
                    "the stream closed before it said who it is",
                ));
            }
            _ if byte[0] == b'\n' => break,
            _ => offered.push(byte[0]),
        }
        if offered.len() > LIMIT {
            return Err(AcpError::transport("the first line is not a token"));
        }
    }
    String::from_utf8(offered).map_err(|_| AcpError::transport("the first line is not text"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn spoken(line: &[u8]) -> Result<(String, Vec<u8>), AcpError> {
        let (mut ours, mut theirs) = tokio::io::duplex(1024);
        theirs.write_all(line).await.expect("the pipe takes it");
        drop(theirs);
        let token = read(&mut ours).await?;
        let mut rest = Vec::new();
        ours.read_to_end(&mut rest).await.expect("the rest");
        Ok((token, rest))
    }

    /// The whole point: what follows the newline is the MCP conversation, and
    /// it must still be there for the loop that is handed the stream.
    #[tokio::test]
    async fn the_line_is_read_and_the_bytes_behind_it_are_left_alone() {
        let (token, rest) = spoken(b"tok-1\n{\"jsonrpc\":\"2.0\"}\n")
            .await
            .expect("a token line");
        assert_eq!(token, "tok-1");
        assert_eq!(rest, b"{\"jsonrpc\":\"2.0\"}\n");
    }

    #[tokio::test]
    async fn a_stream_that_says_nothing_is_not_a_conversation() {
        let refused = spoken(b"").await.expect_err("nothing was said");
        assert!(matches!(refused, AcpError::Transport(_)), "{refused}");
    }

    #[tokio::test]
    async fn a_first_line_that_never_ends_is_refused_rather_than_buffered() {
        let flood = vec![b'x'; LIMIT * 4];
        let refused = spoken(&flood).await.expect_err("that is not a token");
        assert!(refused.to_string().contains("not a token"), "{refused}");
    }

    #[tokio::test]
    async fn what_the_proxy_writes_is_what_the_listener_reads() {
        let (mut ours, mut theirs) = tokio::io::duplex(1024);
        write(&mut theirs, "tok-2").await.expect("it goes");
        assert_eq!(read(&mut ours).await.expect("a token"), "tok-2");
    }
}
