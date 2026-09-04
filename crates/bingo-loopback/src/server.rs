//! The socket: a port on the loopback interface, and one client at a time.
//!
//! Loopback only. What is served is addressed by a secret in its path, so the
//! port is whatever the machine has free — except where the far side was told
//! which ports to expect, which is what [`Loopback::in_range`] is for.

use std::net::{Ipv4Addr, SocketAddr};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::error::LoopbackError;
use crate::request::{END, Head, MAX_BODY, MAX_HEAD, Request};
use crate::response::Response;

/// How much is read from the socket at once.
const CHUNK: usize = 4 * 1024;

/// A bound listener on `127.0.0.1`, and the port it landed on.
#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Whatever port is free. There is nothing to reserve: the path carries
    /// the authority, not the port.
    pub async fn any() -> Result<Self, LoopbackError> {
        Self::bind(0).await
    }

    /// The first free port of `first..first + count`, for a flow whose far side
    /// was told which ports to expect — an OAuth issuer's redirect allow-list.
    pub async fn in_range(first: u16, count: u16) -> Result<Self, LoopbackError> {
        let last_port = first.saturating_add(count.saturating_sub(1));
        let mut last = None;
        for port in first..=last_port {
            match Self::bind(port).await {
                Ok(bound) => return Ok(bound),
                Err(error) => last = Some(error),
            }
        }
        Err(LoopbackError::NoPort(format!(
            "{first}..{last_port}: {}",
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    async fn bind(port: u16) -> Result<Self, LoopbackError> {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = TcpListener::bind(address)
            .await
            .map_err(|e| LoopbackError::Io(format!("bind {address}: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| LoopbackError::Io(format!("read the bound port: {e}")))?
            .port();
        Ok(Self { listener, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// One client. A failure here is the listener's, and fatal to whatever was
    /// being served; what the client then sends is [`Connection::request`]'s.
    pub async fn accept(&self) -> Result<Connection, LoopbackError> {
        let (socket, _) = self
            .listener
            .accept()
            .await
            .map_err(|e| LoopbackError::Io(format!("accept: {e}")))?;
        Ok(Connection { socket })
    }
}

/// One client, until it is answered.
#[derive(Debug)]
pub struct Connection {
    socket: TcpStream,
}

impl Connection {
    /// What the client asked for. `None` is a connection that said nothing —
    /// a browser opens sockets it never sends on — which is neither an error
    /// nor something to answer.
    pub async fn request(&mut self) -> Result<Option<Request>, LoopbackError> {
        let Some((head, rest)) = self.read_head().await? else {
            return Ok(None);
        };
        let head = Head::parse(&head)?;
        let body = self.read_body(&head, rest).await?;
        Ok(Some(Request { head, body }))
    }

    /// Answer, then close. Best effort: a client that has already gone is not
    /// a failure of the thing it asked for.
    pub async fn reply(mut self, response: &Response) {
        let _ = self.socket.write_all(&response.bytes()).await;
        let _ = self.socket.shutdown().await;
    }

    /// Everything up to the blank line, and whatever of the body came with it.
    async fn read_head(&mut self) -> Result<Option<(String, Vec<u8>)>, LoopbackError> {
        let mut read = Vec::new();
        let mut chunk = [0u8; CHUNK];
        loop {
            let n = self
                .socket
                .read(&mut chunk)
                .await
                .map_err(|e| LoopbackError::Io(format!("read the request: {e}")))?;
            if n == 0 {
                return match read.is_empty() {
                    true => Ok(None),
                    false => Err(LoopbackError::Malformed(
                        "the request ended before its head did".into(),
                    )),
                };
            }
            read.extend_from_slice(&chunk[..n]);
            if let Some(end) = read.windows(END.len()).position(|window| window == END) {
                let rest = read.split_off(end + END.len());
                read.truncate(end);
                return Ok(Some((String::from_utf8_lossy(&read).into_owned(), rest)));
            }
            if read.len() > MAX_HEAD {
                return Err(LoopbackError::Malformed(format!(
                    "a head longer than {MAX_HEAD} bytes"
                )));
            }
        }
    }

    /// As much as the head declared, refused before it is read when that is
    /// more than a page may post.
    async fn read_body(&mut self, head: &Head, read: Vec<u8>) -> Result<Vec<u8>, LoopbackError> {
        if head.content_length > MAX_BODY {
            return Err(LoopbackError::TooLarge(head.content_length));
        }
        let mut body = read;
        let mut chunk = [0u8; CHUNK];
        while body.len() < head.content_length {
            let n = self
                .socket
                .read(&mut chunk)
                .await
                .map_err(|e| LoopbackError::Io(format!("read the body: {e}")))?;
            if n == 0 {
                return Err(LoopbackError::Malformed(format!(
                    "the body ended at {} of {} bytes",
                    body.len(),
                    head.content_length
                )));
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(head.content_length);
        Ok(body)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A client that speaks HTTP by hand: `reqwest` would pull `aws-lc-sys`
    /// into this crate's tree and take its Windows cross-check with it.
    pub(crate) async fn send(port: u16, request: &str) -> String {
        send_bytes(port, request.as_bytes()).await
    }

    pub(crate) async fn send_bytes(port: u16, request: &[u8]) -> String {
        let mut socket = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("the server is listening");
        socket
            .write_all(request)
            .await
            .expect("the request is sent");
        // The write half closes so a head that never ends still ends: this is
        // what `Connection: close` means from the client's side.
        socket.shutdown().await.expect("the write half closes");
        let mut answer = Vec::new();
        socket
            .read_to_end(&mut answer)
            .await
            .expect("the server answers");
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// `GET <target>`, with a `Host` a browser would send.
    pub(crate) fn get(target: &str) -> String {
        format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
    }

    pub(crate) fn post(target: &str, body: &str) -> String {
        format!(
            "POST {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    /// Serve exactly one request and hand back what was asked for.
    async fn one(request: &str) -> (Option<Request>, String) {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let request = request.to_string();
        let client = tokio::spawn(async move { send(port, &request).await });
        let mut connection = loopback.accept().await.expect("a client");
        let read = connection.request().await;
        connection.reply(&Response::text("200 OK", "read")).await;
        let answer = client.await.expect("the client task");
        (read.expect("a readable request"), answer)
    }

    #[tokio::test]
    async fn a_bound_port_is_a_real_one_and_a_range_lands_inside_itself() {
        let any = Loopback::any().await.expect("a free port");
        assert_ne!(any.port(), 0);
        let ranged = Loopback::in_range(any.port(), 16)
            .await
            .expect("a free port above the bound one");
        assert!(
            (any.port()..any.port() + 16).contains(&ranged.port()),
            "{} is not in {}..{}",
            ranged.port(),
            any.port(),
            any.port() + 16
        );
        assert_ne!(ranged.port(), any.port(), "the bound one was taken");
    }

    #[tokio::test]
    async fn a_range_with_nothing_free_says_which_ports_were_tried() {
        let held = Loopback::any().await.expect("a free port");
        let error = Loopback::in_range(held.port(), 1).await.err();
        assert!(
            matches!(&error, Some(LoopbackError::NoPort(m)) if m.contains(&held.port().to_string())),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_get_arrives_as_its_method_and_target_and_is_answered() {
        let (request, answer) = one(&get("/tok?x=1")).await;
        let request = request.expect("a request");
        assert_eq!(request.head.method, "GET");
        assert_eq!(request.head.target, "/tok?x=1");
        assert!(request.body.is_empty());
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
    }

    #[tokio::test]
    async fn a_post_arrives_with_the_body_its_length_declared() {
        let (request, _) = one(&post("/tok/answer", r#"{"value":1}"#)).await;
        let request = request.expect("a request");
        assert_eq!(request.head.method, "POST");
        assert_eq!(request.body, br#"{"value":1}"#);
    }

    /// A browser opens sockets it never sends on. One of those must not look
    /// like a client that asked for something.
    #[tokio::test]
    async fn a_connection_that_says_nothing_asks_for_nothing() {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let client = tokio::spawn(async move {
            TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .expect("connect")
        });
        let opened = client.await.expect("the client task");
        drop(opened);
        let mut connection = loopback.accept().await.expect("a client");
        assert_eq!(connection.request().await.expect("no request"), None);
    }

    #[tokio::test]
    async fn a_body_over_the_cap_is_refused_before_it_is_read() {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let head = format!(
            "POST /t/answer HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        );
        let client = tokio::spawn(async move { send(port, &head).await });
        let mut connection = loopback.accept().await.expect("a client");
        let error = connection.request().await.err();
        connection
            .reply(&Response::text("413 Payload Too Large", "too large"))
            .await;
        let _ = client.await;
        assert!(
            matches!(error, Some(LoopbackError::TooLarge(n)) if n == MAX_BODY + 1),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_head_over_the_cap_is_refused() {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let head = format!("GET /{} HTTP/1.1\r\n", "a".repeat(MAX_HEAD + 1));
        let client = tokio::spawn(async move { send(port, &head).await });
        let mut connection = loopback.accept().await.expect("a client");
        let error = connection.request().await.err();
        connection.reply(&Response::not_found()).await;
        let _ = client.await;
        assert!(
            matches!(&error, Some(LoopbackError::Malformed(m)) if m.contains("longer than")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_head_that_never_ends_is_refused_when_the_client_goes() {
        let loopback = Loopback::any().await.expect("a free port");
        let port = loopback.port();
        let client = tokio::spawn(async move { send(port, "GET /t HTTP/1.1\r\n").await });
        let mut connection = loopback.accept().await.expect("a client");
        let error = connection.request().await.err();
        connection.reply(&Response::not_found()).await;
        let _ = client.await;
        assert!(
            matches!(&error, Some(LoopbackError::Malformed(m)) if m.contains("ended before")),
            "got {error:?}"
        );
    }
}
