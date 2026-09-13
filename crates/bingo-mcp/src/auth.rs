//! Which servers can be signed in to, and how a `401` is recognised
//! (ADR-0050 §3).
//!
//! The flows themselves are the library's (`bingo-auth-oauth`, ADR-0012 §1);
//! what is this plugin's is the two questions the library cannot answer:
//! *does this configured server take a sign-in at all*, and *is this rmcp
//! failure the server asking for one*. Both are answered by type, never by
//! looking for digits in a message.

use std::collections::BTreeMap;
use std::sync::Arc;

use bingo_auth_oauth::{CredentialStore, McpAuth};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use rmcp::ServiceError;
use rmcp::service::ClientInitializeError;
use rmcp::transport::streamable_http_client::AuthRequiredError;

use crate::config::Server;

/// One handle per HTTP server bingo may sign in to.
///
/// A stdio server carries its own credentials in its environment, and an HTTP
/// server whose `Authorization` a person wrote themselves has already said
/// who it is — for either, a `401` is a settings problem, not a sign-in bingo
/// can offer (ADR-0050 §3).
pub fn handles(
    servers: &BTreeMap<String, Server>,
    store: Arc<CredentialStore>,
) -> BTreeMap<String, Arc<McpAuth>> {
    let http = reqwest::Client::new();
    servers
        .iter()
        .filter_map(|(name, server)| {
            handle(name, server, Arc::clone(&store), http.clone()).map(|auth| (name.clone(), auth))
        })
        .collect()
}

/// The handle one configured server takes, or nothing when it takes none.
pub fn handle(
    name: &str,
    server: &Server,
    store: Arc<CredentialStore>,
    http: reqwest::Client,
) -> Option<Arc<McpAuth>> {
    let Server::Http {
        url,
        headers,
        oauth,
    } = server
    else {
        return None;
    };
    if headers
        .keys()
        .any(|key| key.eq_ignore_ascii_case("authorization"))
    {
        return None;
    }
    Some(Arc::new(McpAuth::new(
        name,
        url,
        static_headers(headers),
        oauth.as_ref().and_then(|oauth| oauth.client_id.clone()),
        store,
        http,
    )))
}

/// The configured headers as the probe sends them. A header this crate cannot
/// use is dropped rather than reported: the dial reports it, by name, and a
/// probe that refused to run would only hide the `401` behind it.
fn static_headers(headers: &BTreeMap<String, String>) -> HeaderMap {
    let mut built = HeaderMap::new();
    for (name, value) in headers {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes())
            && let Ok(value) = HeaderValue::from_str(value)
        {
            built.insert(name, value);
        }
    }
    built
}

/// The bearer this dial sends, put where the transport reads it. Never into
/// the configured headers, which are the person's own and are forwarded
/// verbatim to foreign agents (ADR-0036 §4).
pub fn bearing(headers: &BTreeMap<String, String>, bearer: &str) -> BTreeMap<String, String> {
    let mut dialled = headers.clone();
    dialled.insert(AUTHORIZATION.to_string(), format!("Bearer {bearer}"));
    dialled
}

/// Whether a handshake failed because the server wants a bearer. rmcp reads
/// the `401` for us: it raises `AuthRequiredError` on the streamable-HTTP
/// transport and keeps it where this can be asked for by type.
pub fn handshake_wants_authorization(error: &ClientInitializeError) -> bool {
    error.is_authorization_required()
}

/// The same question of a call already in flight. `ServiceError` publishes no
/// accessor for it, but the transport error it carries is a public field, so
/// the chain is walked and the type tested — a `401` is never spelled out of
/// a message here.
pub fn call_wants_authorization(error: &ServiceError) -> bool {
    let ServiceError::TransportSend(transport) = error else {
        return false;
    };
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(transport.error.as_ref());
    while let Some(current) = source {
        if current.is::<AuthRequiredError>() {
            return true;
        }
        source = current.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn store() -> Arc<CredentialStore> {
        Arc::new(CredentialStore::new(PathBuf::from(
            "/tmp/bingo-mcp-auth-tests",
        )))
    }

    fn http(headers: &[(&str, &str)]) -> Server {
        Server::Http {
            url: "https://mcp.example.com/mcp".into(),
            headers: headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            oauth: None,
        }
    }

    #[test]
    fn an_http_server_with_no_authorization_of_its_own_can_be_signed_in_to() {
        let auth = handle("remote", &http(&[]), store(), reqwest::Client::new()).expect("a handle");
        assert_eq!(auth.server(), "remote");
    }

    /// ADR-0050 §3: a person's own `Authorization` is their answer to who
    /// this is, and bingo does not sign in over the top of it.
    #[test]
    fn a_server_a_person_already_authorized_takes_no_sign_in() {
        for header in ["Authorization", "authorization", "AUTHORIZATION"] {
            assert!(
                handle(
                    "remote",
                    &http(&[(header, "Bearer mine")]),
                    store(),
                    reqwest::Client::new()
                )
                .is_none(),
                "{header}"
            );
        }
    }

    #[test]
    fn a_child_process_signs_in_to_nothing_here() {
        let stdio = Server::Stdio {
            command: "npx".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            cwd: None,
        };
        assert!(handle("files", &stdio, store(), reqwest::Client::new()).is_none());
    }

    #[test]
    fn only_the_http_servers_that_can_be_signed_in_to_get_a_handle() {
        let servers = BTreeMap::from([
            ("open".to_string(), http(&[])),
            (
                "mine".to_string(),
                http(&[("Authorization", "Bearer mine")]),
            ),
            (
                "files".to_string(),
                Server::Stdio {
                    command: "npx".into(),
                    args: Vec::new(),
                    env: BTreeMap::new(),
                    cwd: None,
                },
            ),
        ]);
        let handles = handles(&servers, store());
        assert_eq!(handles.keys().collect::<Vec<_>>(), ["open"]);
    }

    /// The bearer goes into the dial's headers and leaves the configured ones
    /// where they were: those are what an ACP agent is handed.
    #[test]
    fn the_bearer_joins_the_dial_and_not_the_configured_row() {
        let configured = BTreeMap::from([("X-Tenant".to_string(), "acme".to_string())]);
        let dialled = bearing(&configured, "at_1");
        assert_eq!(dialled["authorization"], "Bearer at_1");
        assert_eq!(dialled["X-Tenant"], "acme");
        assert!(!configured.contains_key("authorization"), "{configured:?}");
    }

    /// The type test, against an error built the way the transport builds one.
    #[test]
    fn a_401_in_the_chain_is_read_by_type_and_anything_else_is_not() {
        let refused =
            ServiceError::TransportSend(rmcp::transport::DynamicTransportError::from_parts(
                "streamable-http",
                std::any::TypeId::of::<()>(),
                Box::new(AuthRequiredError::new(r#"Bearer realm="mcp""#.to_string())),
            ));
        assert!(call_wants_authorization(&refused));
        assert!(!call_wants_authorization(&ServiceError::TransportClosed));
        assert!(!call_wants_authorization(&ServiceError::UnexpectedResponse));
    }
}
