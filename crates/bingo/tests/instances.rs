//! Black-box: what a client sees of a named provider instance (ADR-0017 §2).
//! The instances are written where a person writes them — the user settings
//! layer — and reach the catalogue every `/login` and `/model` completes from.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use bingo_sdk::{AuthStatus, CatalogKind, HostApi};

mod support;

use support::{Server, ready};

const IDLE: &str = r#"{"responses":[{"steps":[{"text":"unused"}]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_lists_each_instance_with_its_own_credential() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    std::fs::write(
        home.path().join(".bingo/settings.json"),
        serde_json::json!({
            "openai": { "instances": { "proxy1": { "apiKey": "sk-one" }, "proxy2": {} } },
            "codex": { "instances": { "work": {} } },
        })
        .to_string(),
    )
    .unwrap();

    let mut server = Server::spawn_at(home, IDLE, &[]);
    let kernel = ready(&mut server).await;
    let listed = kernel.catalog(CatalogKind::Providers).await.unwrap();
    let ids: Vec<&str> = listed.entries.iter().map(|e| e.id.as_str()).collect();
    for id in [
        "fake",
        "anthropic",
        "openai",
        "codex",
        "proxy1",
        "proxy2",
        "work",
    ] {
        assert!(ids.contains(&id), "the catalogue names {id}: {ids:?}");
    }

    let auth = |id: &str| {
        let entry = listed
            .entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("no {id}"));
        serde_json::from_value::<AuthStatus>(entry.meta["auth"].clone()).unwrap()
    };
    assert_eq!(auth("proxy1"), AuthStatus::Ready, "its own `apiKey`");
    assert!(
        matches!(auth("proxy2"), AuthStatus::Missing { hint } if hint.contains("/login proxy2")),
        "an instance with no key of its own says how to give it one: {:?}",
        auth("proxy2")
    );
    assert!(
        matches!(auth("work"), AuthStatus::Missing { hint } if hint.contains("bingo login work")),
        "a subscription instance signs in under its own name: {:?}",
        auth("work")
    );
    kernel.shutdown().await.unwrap();
}
