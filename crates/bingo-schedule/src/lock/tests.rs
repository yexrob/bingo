use super::*;

#[test]
fn the_second_claim_is_refused_until_the_first_drops() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let first = Claim::take(home.path()).expect("the first claim");
    assert!(matches!(
        Claim::take(home.path()),
        Err(ClaimError::WouldBlock { .. })
    ));
    assert!(probe(home.path()).expect("probe the holder"));
    drop(first);
    assert!(!probe(home.path()).expect("probe the released lock"));
    Claim::take(home.path()).expect("the next holder");
}

#[test]
fn releasing_preserves_the_published_marker_and_inode() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let first = Claim::take(home.path()).expect("the first claim");
    let path = first.path().to_path_buf();
    let witness = File::options()
        .read(true)
        .write(true)
        .open(&path)
        .expect("the inode");
    drop(first);
    assert!(path.exists(), "release never unlinks the published path");
    assert_eq!(std::fs::read_to_string(&path).expect("the marker"), MARKER);
    let second = Claim::take(home.path()).expect("the next holder");
    assert!(
        matches!(witness.try_lock(), Err(TryLockError::WouldBlock)),
        "a descriptor opened before release still names the locked inode"
    );
    drop(second);
    assert_eq!(std::fs::read_to_string(path).expect("the marker"), MARKER);
}

#[test]
fn legacy_and_unrecognized_files_are_never_claimed_or_modified() {
    for bytes in [
        b"4242".as_slice(),
        b"",
        b"unknown",
        b"bingo-schedule-runner-v2\n",
        b"\xff",
    ] {
        let home = tempfile::tempdir().expect("a temporary directory");
        let path = home.path().join(LOCK);
        std::fs::write(&path, bytes).expect("a legacy sentinel");
        assert!(matches!(
            Claim::take(home.path()),
            Err(ClaimError::Legacy { .. })
        ));
        assert!(matches!(probe(home.path()), Err(ClaimError::Legacy { .. })));
        assert_eq!(std::fs::read(path).expect("the original sentinel"), bytes);
        assert!(holder(home.path(), false).contains("legacy or unrecognized"));
    }
}

#[test]
fn an_unlocked_modern_file_is_taken_without_rewriting_it() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let path = home.path().join(LOCK);
    std::fs::write(&path, MARKER).expect("a leftover modern marker");
    let claim = Claim::take(home.path()).expect("the OS lock is free");
    assert_eq!(claim.path(), path);
    drop(claim);
    assert_eq!(std::fs::read_to_string(path).expect("the marker"), MARKER);
}

#[test]
fn probing_absent_files_creates_nothing_and_reports_no_owner() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let missing = home.path().join("absent");
    assert!(!probe(&missing).expect("no holder"));
    assert!(!missing.exists());
    assert_eq!(holder(&missing, true), "held by this process");
    assert_eq!(
        holder(&missing, false),
        "standby — no runner holds this store; waiting to take over"
    );
}

#[test]
fn holder_says_another_runner_only_while_the_os_lock_is_held() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let claim = Claim::take(home.path()).expect("the claim");
    let said = holder(home.path(), false);
    assert!(said.starts_with("standby — another runner holds"), "{said}");
    assert!(said.contains("runner.lock"), "{said}");
    assert!(!said.contains("remove"), "{said}");
    drop(claim);
    assert!(holder(home.path(), false).starts_with("standby — no runner holds"));
}

#[test]
fn an_io_failure_is_not_contention_or_legacy() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let file = home.path().join("not-a-directory");
    std::fs::write(&file, "data").expect("a regular file");
    assert!(matches!(Claim::take(&file), Err(ClaimError::Io { .. })));
    assert!(matches!(probe(&file), Err(ClaimError::Io { .. })));
    assert!(holder(&file, false).starts_with("standby — cannot inspect"));
}

#[test]
fn initialization_publishes_only_one_complete_marker_under_contention() {
    let home = tempfile::tempdir().expect("a temporary directory");
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    Claim::take(home.path())
                })
            })
            .collect();
        let claims: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("worker"))
            .collect();
        assert_eq!(claims.iter().filter(|c| c.is_ok()).count(), 1);
        assert!(
            claims
                .iter()
                .all(|c| c.is_ok() || matches!(c, Err(ClaimError::WouldBlock { .. })))
        );
        drop(claims);
        assert_eq!(
            std::fs::read_to_string(home.path().join(LOCK)).expect("the published marker"),
            MARKER
        );
    });
    let paths: Vec<_> = std::fs::read_dir(home.path())
        .expect("the directory")
        .map(|e| e.expect("a directory entry").file_name())
        .collect();
    assert_eq!(
        paths,
        [std::ffi::OsString::from(LOCK)],
        "staging files are cleaned up"
    );
}
