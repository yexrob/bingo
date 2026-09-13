//! Finding the authorization server a protected resource names (ADR-0050 §1).
//!
//! Two ladders and two refusals. RFC 9728 says where a resource keeps its
//! metadata; RFC 8414 and OIDC say where a server keeps its own, and the
//! order of those URLs is the specification's, not a guess. The refusals are
//! what keeps a discovered issuer from being anybody at all: the metadata
//! document must call itself by the name the URL was built from, and a
//! server that does not offer PKCE S256 is not signed in to.
//!
//! Read it bottom up: [`split`], [`resource_of`], [`protected_resource_urls`]
//! and [`metadata_urls`] are pure and tested without a socket; [`discover`] is
//! the only place they meet a network.

use serde::Deserialize;
use serde_json::Value;

use crate::challenge::Challenge;
use crate::error::AuthError;
use crate::issuer::Issuer;

/// An authorization server bingo may sign in to, and what it mints for.
#[derive(Clone, Debug)]
pub struct Discovered {
    /// The endpoints, with no client id yet: registration fills that in.
    pub issuer: Issuer,
    /// RFC 7591's endpoint, when the server offers dynamic registration.
    pub registration_endpoint: Option<String>,
    /// The scopes the resource advertises, joined as a `scope` value.
    pub scopes_supported: Option<String>,
}

/// The resource's own metadata (RFC 9728 §3).
#[derive(Debug, Default, Deserialize)]
struct ProtectedResource {
    #[serde(default)]
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

/// From an MCP server URL and whatever its `401` said, the server to sign in
/// to. `challenge` may name nothing: the well-known ladder covers a resource
/// on the 2025-06-18 revision.
pub async fn discover(
    http: &reqwest::Client,
    server_url: &str,
    challenge: &Challenge,
) -> Result<Discovered, AuthError> {
    let resource = protected_resource(http, server_url, challenge).await?;
    let authorization_server = resource.authorization_servers.first().ok_or_else(|| {
        AuthError::Invalid(format!(
            "{server_url} names no authorization server to sign in to"
        ))
    })?;
    let metadata = server_metadata(http, authorization_server).await?;
    let audience = resource_of(resource.resource.as_deref(), server_url);
    Ok(Discovered {
        issuer: issuer_of(&metadata, authorization_server, audience)?,
        registration_endpoint: string(&metadata, "registration_endpoint"),
        scopes_supported: scope_of(challenge, &resource),
    })
}

/// The endpoints of an authorization server already known by name — what a
/// stored entry needs to renew itself. The entry keeps the issuer and derives
/// the rest from it, rather than keeping a copy of every endpoint that would
/// go stale the day the server moves one.
pub async fn endpoints(
    http: &reqwest::Client,
    authorization_server: &str,
    resource: String,
) -> Result<Issuer, AuthError> {
    let metadata = server_metadata(http, authorization_server).await?;
    issuer_of(&metadata, authorization_server, resource)
}

/// The scope to ask for: what the `401` named, else what the resource
/// advertises, else none — asking for a scope nobody offered is a refusal.
fn scope_of(challenge: &Challenge, resource: &ProtectedResource) -> Option<String> {
    challenge.scope.clone().or_else(|| {
        (!resource.scopes_supported.is_empty()).then(|| resource.scopes_supported.join(" "))
    })
}

/// The endpoints and the audience, once the document has proved it is the
/// server the URL claimed and that it can do PKCE S256.
fn issuer_of(
    metadata: &Value,
    authorization_server: &str,
    resource: String,
) -> Result<Issuer, AuthError> {
    check_issuer(metadata, authorization_server)?;
    check_s256(metadata, authorization_server)?;
    Ok(Issuer {
        client_id: String::new(),
        base: authorization_server.trim_end_matches('/').to_string(),
        authorize_path: endpoint(metadata, "authorization_endpoint")?,
        token_path: endpoint(metadata, "token_endpoint")?,
        revoke_path: string(metadata, "revocation_endpoint"),
        device: None,
        scope: String::new(),
        resource: Some(resource),
        form_encoded: true,
        authorize_extra: Vec::new(),
    })
}

/// The document must call itself by the name the URL was built from,
/// otherwise a resource could point at a server that hands out tokens in
/// somebody else's name. Compared with the trailing slash trimmed off both,
/// which is the one difference two spellings of the same issuer have.
fn check_issuer(metadata: &Value, authorization_server: &str) -> Result<(), AuthError> {
    let named = string(metadata, "issuer").unwrap_or_default();
    if named.trim_end_matches('/') == authorization_server.trim_end_matches('/') {
        return Ok(());
    }
    Err(AuthError::Invalid(format!(
        "the metadata at {authorization_server} calls itself {named}"
    )))
}

/// PKCE S256 is mandatory for MCP. A server that does not say it supports it
/// is refused rather than tried: the alternative is a flow whose only
/// protection is a redirect nobody guards.
fn check_s256(metadata: &Value, authorization_server: &str) -> Result<(), AuthError> {
    let offered = metadata
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method == "S256"));
    if offered {
        return Ok(());
    }
    Err(AuthError::Invalid(format!(
        "{authorization_server} does not offer PKCE S256, which MCP requires"
    )))
}

/// RFC 8707's audience: the server URL with the fragment gone and the path
/// kept. The resource's own `resource` wins when it covers the URL, because
/// it is the name the server knows itself by.
fn resource_of(named: Option<&str>, server_url: &str) -> String {
    let url = server_url.split('#').next().unwrap_or(server_url);
    let url = url.trim_end_matches('/');
    match named {
        Some(named) if covers(named, url) => named.trim_end_matches('/').to_string(),
        _ => url.to_string(),
    }
}

/// Whether the resource identifier the document names is this URL or a
/// prefix of it — `https://host/api` covers `https://host/api/mcp`.
fn covers(named: &str, url: &str) -> bool {
    let named = named.trim_end_matches('/');
    url == named || url.starts_with(&format!("{named}/"))
}

async fn protected_resource(
    http: &reqwest::Client,
    server_url: &str,
    challenge: &Challenge,
) -> Result<ProtectedResource, AuthError> {
    let urls = protected_resource_urls(server_url, challenge)?;
    for url in &urls {
        if let Some(document) = fetch(http, url).await
            && let Ok(resource) = serde_json::from_value::<ProtectedResource>(document)
            && !resource.authorization_servers.is_empty()
        {
            return Ok(resource);
        }
    }
    Err(AuthError::Invalid(format!(
        "no protected-resource metadata for {server_url} (tried {})",
        urls.join(", ")
    )))
}

async fn server_metadata(
    http: &reqwest::Client,
    authorization_server: &str,
) -> Result<Value, AuthError> {
    let urls = metadata_urls(authorization_server)?;
    for url in &urls {
        if let Some(document) = fetch(http, url).await {
            return Ok(document);
        }
    }
    Err(AuthError::Invalid(format!(
        "no authorization-server metadata for {authorization_server} (tried {})",
        urls.join(", ")
    )))
}

/// One rung of a ladder: a document, or nothing to say about it. A rung that
/// 404s is the specification working, not a failure to report.
async fn fetch(http: &reqwest::Client, url: &str) -> Option<Value> {
    let response = http.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json::<Value>().await.ok()
}

/// Where a resource keeps its metadata: what the challenge named, then the
/// well-known URL with the endpoint's path inserted after the segment, then
/// the bare well-known URL (RFC 9728 §3.1).
pub fn protected_resource_urls(
    server_url: &str,
    challenge: &Challenge,
) -> Result<Vec<String>, AuthError> {
    let (origin, path) = split(server_url)?;
    let mut urls = Vec::new();
    if let Some(named) = &challenge.resource_metadata {
        urls.push(named.clone());
    }
    if !path.is_empty() {
        urls.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    urls.push(format!("{origin}/.well-known/oauth-protected-resource"));
    urls.dedup();
    Ok(urls)
}

/// Where an authorization server keeps its own: the path-inserted spellings
/// first, then the path-appended OIDC one (RFC 8414 §3.1, OIDC Discovery).
pub fn metadata_urls(authorization_server: &str) -> Result<Vec<String>, AuthError> {
    let (origin, path) = split(authorization_server)?;
    if path.is_empty() {
        return Ok(vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]);
    }
    Ok(vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
        format!("{origin}{path}/.well-known/openid-configuration"),
    ])
}

/// `https://host:port/path` → the origin and the path, the path without its
/// trailing slash and without a query or a fragment. Hand-written because
/// `url` is not a dependency of this crate and this is what is needed of it.
fn split(url: &str) -> Result<(&str, &str), AuthError> {
    let after_scheme = crate::issuer::is_absolute(url)
        .then(|| url.split_once("://"))
        .flatten()
        .map(|(_, rest)| rest)
        .ok_or_else(|| AuthError::Invalid(format!("{url} is not an http url")))?;
    let end = url.len() - after_scheme.len();
    let trimmed = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .trim_end_matches('/');
    match trimmed[end..].find('/') {
        Some(at) => Ok(trimmed.split_at(end + at)),
        None => Ok((trimmed, "")),
    }
}

fn endpoint(metadata: &Value, key: &str) -> Result<String, AuthError> {
    string(metadata, key).ok_or_else(|| AuthError::Invalid(format!("the metadata names no {key}")))
}

fn string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_url_splits_into_its_origin_and_its_path() {
        assert_eq!(
            split("https://mcp.example.com/api/mcp").expect("a url"),
            ("https://mcp.example.com", "/api/mcp")
        );
        assert_eq!(
            split("https://mcp.example.com").expect("a url"),
            ("https://mcp.example.com", "")
        );
        assert_eq!(
            split("http://127.0.0.1:8931/mcp/").expect("a url"),
            ("http://127.0.0.1:8931", "/mcp")
        );
        assert_eq!(
            split("https://h/a?k=v#f").expect("a url"),
            ("https://h", "/a")
        );
        assert!(matches!(split("ftp://h/a"), Err(AuthError::Invalid(_))));
    }

    /// RFC 9728 §3.1: the path is *inserted* after the well-known segment,
    /// not appended to the URL. Getting this backwards is the one mistake
    /// that makes discovery fail against every real server.
    #[test]
    fn the_resource_ladder_inserts_the_path_after_the_well_known_segment() {
        assert_eq!(
            protected_resource_urls("https://mcp.example.com/api/mcp", &Challenge::default())
                .expect("a ladder"),
            [
                "https://mcp.example.com/.well-known/oauth-protected-resource/api/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource",
            ]
        );
    }

    #[test]
    fn a_challenge_that_names_the_document_puts_it_first() {
        let challenge = Challenge {
            resource_metadata: Some("https://elsewhere.example/prm".into()),
            ..Challenge::default()
        };
        let urls =
            protected_resource_urls("https://mcp.example.com/mcp", &challenge).expect("a ladder");
        assert_eq!(urls[0], "https://elsewhere.example/prm");
        assert_eq!(urls.len(), 3);
    }

    #[test]
    fn a_server_at_the_root_has_one_well_known_url_and_not_two() {
        assert_eq!(
            protected_resource_urls("https://mcp.example.com", &Challenge::default())
                .expect("a ladder"),
            ["https://mcp.example.com/.well-known/oauth-protected-resource"]
        );
    }

    /// RFC 8414 §3.1 and OIDC Discovery: three spellings for an issuer with a
    /// path, in this order; two for one without.
    #[test]
    fn the_server_ladder_is_the_specifications_order() {
        assert_eq!(
            metadata_urls("https://as.example.com/tenant").expect("a ladder"),
            [
                "https://as.example.com/.well-known/oauth-authorization-server/tenant",
                "https://as.example.com/.well-known/openid-configuration/tenant",
                "https://as.example.com/tenant/.well-known/openid-configuration",
            ]
        );
        assert_eq!(
            metadata_urls("https://as.example.com/").expect("a ladder"),
            [
                "https://as.example.com/.well-known/oauth-authorization-server",
                "https://as.example.com/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn the_audience_is_the_url_unless_the_resource_names_one_that_covers_it() {
        assert_eq!(
            resource_of(None, "https://mcp.example.com/api/mcp#frag"),
            "https://mcp.example.com/api/mcp"
        );
        assert_eq!(
            resource_of(
                Some("https://mcp.example.com/api"),
                "https://mcp.example.com/api/mcp"
            ),
            "https://mcp.example.com/api",
            "a prefix the server knows itself by is sent verbatim"
        );
        assert_eq!(
            resource_of(
                Some("https://other.example/"),
                "https://mcp.example.com/api/mcp"
            ),
            "https://mcp.example.com/api/mcp",
            "an identifier that covers nothing here is not ours to send"
        );
    }

    fn metadata(issuer: &str) -> Value {
        json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "registration_endpoint": format!("{issuer}/register"),
            "revocation_endpoint": format!("{issuer}/revoke"),
            "code_challenge_methods_supported": ["S256"],
        })
    }

    /// Mount the two ladders' last rungs and the resource, so the whole of
    /// discovery runs against one origin.
    async fn resource_server(server: &MockServer, prm: Value, metadata: Value) {
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(prm))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(metadata))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_resource_leads_to_its_server_and_the_audience_it_names() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        resource_server(
            &server,
            json!({
                "resource": format!("{issuer}/mcp"),
                "authorization_servers": [issuer],
                "scopes_supported": ["mcp:tools", "mcp:read"],
            }),
            metadata(&issuer),
        )
        .await;
        let found = discover(
            &reqwest::Client::new(),
            &format!("{issuer}/mcp"),
            &Challenge::default(),
        )
        .await
        .expect("a discovery");
        assert_eq!(found.issuer.base, issuer);
        assert_eq!(found.issuer.authorize_path, format!("{issuer}/authorize"));
        assert_eq!(found.issuer.token_path, format!("{issuer}/token"));
        assert_eq!(
            found.issuer.revoke_path.as_deref(),
            Some(format!("{issuer}/revoke").as_str())
        );
        assert_eq!(
            found.registration_endpoint.as_deref(),
            Some(format!("{issuer}/register").as_str())
        );
        assert_eq!(
            found.scopes_supported.as_deref(),
            Some("mcp:tools mcp:read")
        );
        assert_eq!(
            found.issuer.resource.as_deref(),
            Some(format!("{issuer}/mcp").as_str())
        );
        assert!(
            found.issuer.device.is_none(),
            "no device flow is discovered"
        );
    }

    /// The challenge's `scope` is the server's word about *this* refusal and
    /// wins over the resource's whole advertised list.
    #[tokio::test]
    async fn the_scope_the_challenge_named_wins_over_the_advertised_list() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        resource_server(
            &server,
            json!({ "authorization_servers": [issuer], "scopes_supported": ["a", "b"] }),
            metadata(&issuer),
        )
        .await;
        let challenge = Challenge {
            scope: Some("mcp:tools".into()),
            ..Challenge::default()
        };
        let found = discover(
            &reqwest::Client::new(),
            &format!("{issuer}/mcp"),
            &challenge,
        )
        .await
        .expect("a discovery");
        assert_eq!(found.scopes_supported.as_deref(), Some("mcp:tools"));
    }

    #[tokio::test]
    async fn a_document_that_calls_itself_something_else_is_refused() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        resource_server(
            &server,
            json!({ "authorization_servers": [issuer] }),
            metadata("https://impostor.example.com"),
        )
        .await;
        let refused = discover(
            &reqwest::Client::new(),
            &format!("{issuer}/mcp"),
            &Challenge::default(),
        )
        .await
        .expect_err("an impostor");
        assert!(
            refused.to_string().contains("impostor.example.com"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_server_without_s256_is_refused_in_words() {
        let server = MockServer::start().await;
        let issuer = server.uri();
        for methods in [json!(["plain"]), Value::Null] {
            let server = MockServer::start().await;
            let issuer_uri = server.uri();
            let mut document = metadata(&issuer_uri);
            match methods {
                Value::Null => {
                    document
                        .as_object_mut()
                        .expect("an object")
                        .remove("code_challenge_methods_supported");
                }
                ref offered => document["code_challenge_methods_supported"] = offered.clone(),
            }
            resource_server(
                &server,
                json!({ "authorization_servers": [issuer_uri] }),
                document,
            )
            .await;
            let refused = discover(
                &reqwest::Client::new(),
                &format!("{}/mcp", server.uri()),
                &Challenge::default(),
            )
            .await
            .expect_err("no S256");
            assert!(refused.to_string().contains("S256"), "{refused}");
        }
        drop(issuer);
    }

    #[tokio::test]
    async fn a_resource_with_no_metadata_anywhere_says_what_it_tried() {
        let server = MockServer::start().await;
        let refused = discover(
            &reqwest::Client::new(),
            &format!("{}/mcp", server.uri()),
            &Challenge::default(),
        )
        .await
        .expect_err("nothing is mounted");
        assert!(
            refused
                .to_string()
                .contains(".well-known/oauth-protected-resource/mcp"),
            "{refused}"
        );
    }
}
