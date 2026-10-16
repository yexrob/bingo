use super::*;
use bingo_sdk::testing::ServiceHost;
struct Owner;
#[async_trait]
impl AgentsControl for Owner {
    async fn roster(&self, request: Session) -> Result<Vec<AgentRow>, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok(vec![])
    }
    async fn team(&self, request: Session) -> Result<Vec<TeamRow>, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok(serde_json::from_str(include_str!("../../fixtures/agents_control/team.json")).unwrap())
    }
    async fn stop(&self, request: Named) -> Result<String, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok("stop accepted".into())
    }
    async fn dismiss(&self, request: Named) -> Result<String, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok("dismiss accepted".into())
    }
    async fn message(&self, request: Message) -> Result<String, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok("message accepted".into())
    }
    async fn reopen(&self, request: Message) -> Result<String, ServiceError> {
        assert_eq!(request.session.as_str(), "ses_parent");
        Ok("reopen accepted".into())
    }
}
#[tokio::test]
async fn native_and_wire_find_preserve_every_fixture() {
    let fixtures: Vec<Value> =
        serde_json::from_str(include_str!("../../fixtures/agents_control/verbs.json")).unwrap();
    let owner: Arc<dyn AgentsControl> = Arc::new(Owner);
    let native = ServiceHost::holding(&KEY, owner.clone());
    assert!(Arc::ptr_eq(&find(&native).unwrap(), &owner));
    assert!(find(&ServiceHost::handle()).is_none());
    let adapter = wire(owner);
    let host = ServiceHost::handle();
    host.open_service(KEY.name(), adapter.clone()).unwrap();
    let roundtrip = wire(find(&host).unwrap());
    for fixture in fixtures {
        let method = fixture["method"].as_str().unwrap();
        for face in [&adapter, &roundtrip] {
            assert_eq!(
                face.call(method, fixture["params"].clone()).await.unwrap(),
                fixture["answer"]
            );
            assert!(face.call(method, Value::Null).await.is_err());
        }
    }
    assert!(adapter.call("unknown", Value::Null).await.is_err());
    let face = adapter.face().unwrap();
    assert!(face.methods.iter().all(|m| m.entry.pinned == ["/session"]));
}

#[test]
fn a_roster_row_has_one_source_of_name_and_activity() {
    let expected: Value =
        serde_json::from_str(include_str!("../../fixtures/agents_control/row.json")).unwrap();
    let row: AgentRow = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(serde_json::to_value(row).unwrap(), expected);
    assert!(expected.get("name").is_none());
    assert!(expected.get("state").is_none());
}

#[test]
fn every_person_only_write_is_unsafe_on_the_wire() {
    let face = wire(Arc::new(Owner)).face().unwrap();
    for method in face.methods {
        assert_eq!(
            method.unsafe_call,
            !matches!(
                method.entry.name.as_ref(),
                "agents-control.roster" | "agents-control.team"
            )
        );
    }
}

#[test]
fn team_rows_keep_declared_roles_and_derive_seating_from_summaries() {
    let expected: Value =
        serde_json::from_str(include_str!("../../fixtures/agents_control/team.json")).unwrap();
    let rows: Vec<TeamRow> = serde_json::from_value(expected.clone()).unwrap();
    assert_eq!(rows[0].role, "reviewer");
    assert!(rows[0].summary.is_some());
    assert_eq!(rows[1].role, "scout");
    assert!(rows[1].summary.is_none());
    assert_eq!(serde_json::to_value(rows).unwrap(), expected);
    assert!(expected[0].get("state").is_none());
}
