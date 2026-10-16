#![allow(clippy::unwrap_used, clippy::expect_used)]

use bingo_sdk::CommandFold;
use serde_json::json;

#[test]
fn a_command_fold_has_one_wire_shape() {
    let fixture = json!({"into":"panel","names":["first","second"]});
    let fold: CommandFold = serde_json::from_value(fixture.clone()).unwrap();
    assert_eq!(fold.into, "panel");
    assert_eq!(fold.names, ["first", "second"]);
    assert_eq!(serde_json::to_value(fold).unwrap(), fixture);
}

#[test]
fn a_command_fold_schema_requires_the_destination_and_names() {
    let schema = serde_json::to_value(schemars::schema_for!(CommandFold)).unwrap();
    assert_eq!(schema["properties"]["into"]["type"], "string");
    assert_eq!(schema["properties"]["names"]["type"], "array");
    assert_eq!(schema["properties"]["names"]["items"]["type"], "string");
    assert_eq!(schema["required"], json!(["into", "names"]));
    for invalid in [json!({"into":"panel"}), json!({"names":["first"]})] {
        assert!(serde_json::from_value::<CommandFold>(invalid).is_err());
    }
}
