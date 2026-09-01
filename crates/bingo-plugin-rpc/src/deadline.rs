//! How long a crossing may take, in one place (ADR-0030 §4).
//!
//! Every constant here bounds one wait on a process that may be slow, wedged
//! or already gone. They differ by what is lost when one runs out — a plugin,
//! a round's pieces, a compaction — and each is spent where the loss is
//! cheapest to explain to the person watching.

use std::time::Duration;

/// How long a process has to spawn and answer `initialize`. A plugin is a
/// local process the person installed; one that cannot say what it is in this
/// long is broken, and waiting longer only delays the session.
pub const HANDSHAKE: Duration = Duration::from_secs(10);

/// How long a contributor has to answer `context/contribute`. The shortest of
/// the three, because it is spent on every round of every turn: past it the
/// round goes on without that contributor's pieces and a notice says whose
/// deadline was missed. The turn never waits on a dead process.
pub const CONTRIBUTE: Duration = Duration::from_secs(3);

/// How long a compaction has. The longest, because a strategy that asks a
/// model of its own is paying for a whole response — the provider crates wait
/// this long for one too. Past it the call fails with the error the trait
/// already speaks, and the kernel's breaker counts it like any other failure.
pub const COMPACT: Duration = Duration::from_secs(60);

/// How long a running stream may say nothing. The longest, because it bounds
/// one silence and not a whole response: a model that thinks before its first
/// token is quiet for a long while, and cutting a legitimate stream off is
/// worse than waiting. Past it the stream yields the timeout the kernel already
/// retries on, so a process that ignored `provider/cancel`, wedged, or went
/// quiet without closing its pipe cannot hold a turn open.
pub const PROVIDER_IDLE: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;

    /// The order is the point: the hot path waits least, the model waits most.
    #[test]
    fn the_hot_path_has_the_shortest_deadline_of_them_all() {
        assert!(CONTRIBUTE < HANDSHAKE);
        assert!(HANDSHAKE < COMPACT);
        assert!(
            COMPACT < PROVIDER_IDLE,
            "a whole compaction is bounded more tightly than one silence"
        );
    }
}
