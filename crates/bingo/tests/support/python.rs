//! Whether the suites that drive a plugin written in Python can run here.
//!
//! Shared by every test binary that needs the interpreter, so the decision is
//! made once: `#[path = ".../support/python.rs"] mod python;`.

use std::process::{Command, Stdio};

/// Whether a plugin written in Python can run here.
///
/// A developer's machine without `python3` skips those tests, and says so. CI
/// does not get to: a suite that passed because its interpreter was missing
/// tested nothing, and the green tick it leaves behind is a lie. So where `CI`
/// is set, an absent interpreter is a failure carrying the reason.
pub fn python3() -> bool {
    let there = Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    assert!(
        there || std::env::var_os("CI").is_none(),
        "python3 is not on PATH and CI is set: these tests would have skipped \
         themselves into a passing run. Install python3 on the runner."
    );
    there
}
