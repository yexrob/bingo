use super::selection::*;

#[test]
fn the_record_contract_is_a_versioned_provider_model_pair() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("last-model.json");
    std::fs::write(
        &path,
        r#"{"version":1,"provider":"proxy","model":"org/model"}"#,
    )
    .unwrap();
    let choice = load(&path).unwrap();
    assert_eq!(choice.provider, "proxy");
    assert_eq!(choice.model, "org/model");
    save(&path, &choice).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap(),
        serde_json::json!({"version":1,"provider":"proxy","model":"org/model"})
    );
}

#[test]
fn absent_corrupt_incomplete_and_future_records_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("last-model.json");
    assert!(load(&path).is_none());
    for text in [
        "{",
        "null",
        r#"{"provider":"p"}"#,
        r#"{"version":2,"provider":"p","model":"m"}"#,
        r#"{"version":1,"provider":"","model":"m"}"#,
        r#"{"version":1,"provider":"p","model":" "}"#,
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(load(&path).is_none(), "{text}");
    }
}

#[test]
fn concurrent_writes_publish_whole_pairs_and_leave_no_temporary_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/last-model.json");
    std::thread::scope(|scope| {
        for n in 0..12 {
            let path = &path;
            scope.spawn(move || {
                save(path, &Selection::new(format!("p{n}"), format!("m{n}"))).unwrap()
            });
        }
    });
    let choice = load(&path).unwrap();
    assert_eq!(choice.provider[1..], choice.model[1..]);
    assert_eq!(
        std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
        1
    );
}

#[test]
fn a_failed_publish_cleans_up_its_temporary_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("last-model.json");
    std::fs::create_dir(&path).unwrap();
    assert!(save(&path, &Selection::new("p".into(), "m".into())).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
