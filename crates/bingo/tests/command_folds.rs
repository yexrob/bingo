//! The real built-in owners can be switched while a client keeps its session.
use crate::support::{LIMIT, Server, ack_for, create, ready, who};
use bingo_sdk::{
    Attachment, Catalog, CatalogKind, HostApi, Input, IntentId, IntentOutcome, OpenOptions, Origin,
    SessionId, render,
};
use std::collections::BTreeMap;

struct Fold {
    owner: &'static str,
    into: &'static str,
    names: &'static [&'static str],
    owns_command: bool,
}
const FOLDS: &[Fold] = &[
    Fold {
        owner: "bingo.mod.crew",
        into: "crew",
        names: &["agents", "tasks", "room", "team"],
        owns_command: true,
    },
    Fold {
        owner: "bingo.mod.browse",
        into: "browse",
        names: &["rewind"],
        owns_command: true,
    },
    Fold {
        owner: "bingo.mod.memory",
        into: "memory",
        names: &["experience"],
        owns_command: false,
    },
    Fold {
        owner: "bingo.mod.schedule",
        into: "schedule",
        names: &["wake"],
        owns_command: false,
    },
    Fold {
        owner: "bingo.mod.models",
        into: "model",
        names: &["models", "provider"],
        owns_command: false,
    },
    Fold {
        owner: "bingo.mod.plugins",
        into: "plugins",
        names: &["mcp"],
        owns_command: false,
    },
];

fn check(catalog: &Catalog, disabled: Option<&str>) {
    let expected: BTreeMap<_, _> = FOLDS
        .iter()
        .filter(|fold| Some(fold.owner) != disabled)
        .flat_map(|fold| fold.names.iter().map(|name| (*name, fold.into)))
        .collect();
    let actual: BTreeMap<_, _> = catalog
        .entries
        .iter()
        .filter_map(|entry| {
            entry.meta.get("foldedInto").map(|into| {
                (
                    entry.id.as_str(),
                    into.as_str().expect("a fold names its destination"),
                )
            })
        })
        .collect();
    assert_eq!(actual, expected, "disabled: {disabled:?}");
    for fold in FOLDS {
        let destination = catalog.entries.iter().any(|entry| entry.id == fold.into);
        assert_eq!(
            destination,
            Some(fold.owner) != disabled || !fold.owns_command,
            "{} destination with {disabled:?} disabled",
            fold.into
        );
        for name in fold.names {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == *name)
                .unwrap_or_else(|| panic!("/{name} disappeared with {disabled:?} disabled"));
            assert_eq!(entry.meta["name"], *name);
            if Some(fold.owner) == disabled {
                assert!(entry.meta.get("foldedInto").is_none(), "{name}");
            }
            if *name == "room" {
                assert_eq!(entry.meta["aliases"], serde_json::json!(["rooms"]));
            }
        }
    }
}

async fn catalogues(host: &impl HostApi, session: &SessionId, disabled: Option<&str>) {
    check(
        &host.catalog(CatalogKind::Commands).await.unwrap(),
        disabled,
    );
    check(
        &host
            .catalog_for(session, CatalogKind::Commands)
            .await
            .unwrap(),
        disabled,
    );
}

async fn switch(session: &mut Attachment, owner: &str, enabled: bool) {
    let verb = if enabled { "enable" } else { "disable" };
    let intent = IntentId::mint();
    session.handle.submit(
        intent.clone(),
        Input::text(
            format!("/plugins {verb} {owner}"),
            Origin::surface("rpc").on(render::SurfaceName::Text),
        ),
    );
    let outcome = ack_for(session, &intent).await;
    let IntentOutcome::Applied { result } = outcome else {
        panic!("{owner} {verb}: {outcome:?}");
    };
    assert_eq!(
        result["message"],
        format!("{owner} is {} now.", if enabled { "on" } else { "off" })
    );
    assert!(
        result.get("surface").is_none(),
        "plain callers do not open a panel: {result}"
    );
    assert!(!session.snapshot.busy());
}

#[tokio::test(flavor = "multi_thread")]
async fn each_builtin_fold_tracks_its_owner_live_over_rpc() {
    let mut server = Server::spawn(r#"{"responses":[]}"#);
    let host = ready(&mut server).await;
    let mut session = host
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    catalogues(&host, &session.session, None).await;
    for fold in FOLDS {
        switch(&mut session, fold.owner, false).await;
        catalogues(&host, &session.session, Some(fold.owner)).await;
        switch(&mut session, fold.owner, true).await;
        catalogues(&host, &session.session, None).await;
    }
    host.shutdown().await.unwrap();
    assert!(
        tokio::time::timeout(LIMIT, server.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
