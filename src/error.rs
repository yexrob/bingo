//! Error code contract (single source of truth, D-feedback).
//!
//! Spec: `notes/design/feedback-states.md` §4.3 (C exit mapping):
//! - Every module error enum implements [`ErrorCode`]: match **exhausts all variants,
//!   no `_` arm**; variants without a stable code yet **explicitly return [`GENERIC`]**
//!   (explicit behavior, not an implicit fallback).
//! - Exits (CLI logging / TUI rendering) share [`map_error`]; no per-exit mapping.
//! - Code values are **semver**: once released, only add, never modify or reuse;
//!   a new code = new variant mapping + a new assertion in the drift-guard tests,
//!   CI goes red if either is missing.

/// Published stable code: this path has no stable code assigned yet (error semantics
/// degrade to generic).
pub const GENERIC: &str = "GENERIC";

/// Stable UI-level codes (not Rust error types): slash command errors embed
/// `[error] code=<CODE> msg=…` in the transient row (design contract §4.5);
/// qa asserts on `code=` only — copy stays changeable.
/// Unknown slash command (falls to the guidance line).
pub const SLASH_ERROR_UNKNOWN_COMMAND: &str = "UNKNOWN_COMMAND";
/// Invalid slash argument (usage line shown, state unchanged).
pub const SLASH_ERROR_BAD_ARGUMENT: &str = "BAD_ARGUMENT";
/// The turn's task ended without reporting an outcome (a panic inside the spawn).
/// Distinct from `SERVER_ERROR`: nothing was wrong upstream, the harness lost the turn.
pub const TURN_LOST: &str = "TURN_LOST";
/// Something went wrong inside a run that did not end it — the engine's own
/// warning, raised as feedback so a frontend branches on the code rather than on
/// the sentence.
pub const RUNTIME_WARNING: &str = "RUNTIME_WARNING";
/// Mail is waiting for the main agent and a digest turn has not run yet — the
/// "reading the mail" state a frontend shows while the debounce window runs.
pub const MAIL_WAITING: &str = "MAIL_WAITING";

/// Stable app-server codes (`notes/design/gui-app-server.md` §Errors): the
/// JSON-RPC `error.data.bingoCode` a client branches on. They live here rather
/// than in the protocol module because there is one code registry, not one per
/// exit; `crate::app_server::protocol::ProtocolErrorKind` maps each to its
/// JSON-RPC number, scope, and recoverability, and its drift guard asserts the
/// pairing.
/// The client asked for a protocol major this build does not speak.
pub const PROTOCOL_UNSUPPORTED: &str = "PROTOCOL_UNSUPPORTED";
/// The client cannot answer interactions, so it may not control a session.
pub const CAPABILITY_REQUIRED: &str = "CAPABILITY_REQUIRED";
/// A call arrived before `initialize` completed.
pub const NOT_INITIALIZED: &str = "NOT_INITIALIZED";
/// `initialize` arrived twice on one connection.
pub const ALREADY_INITIALIZED: &str = "ALREADY_INITIALIZED";
/// A session-scoped call arrived with no session open.
pub const NO_ACTIVE_SESSION: &str = "NO_ACTIVE_SESSION";
pub const SESSION_NOT_FOUND: &str = "SESSION_NOT_FOUND";
pub const CONVERSATION_NOT_FOUND: &str = "CONVERSATION_NOT_FOUND";
/// The turn already reached its terminal state.
pub const TURN_CLOSED: &str = "TURN_CLOSED";
/// The item cursor was issued under an older `historyGeneration`.
pub const STALE_PAGE: &str = "STALE_PAGE";
/// The precondition revision no longer matches the resource.
pub const STALE_REVISION: &str = "STALE_REVISION";
/// Keyboard approval arrived inside the confirmation guard (D81).
pub const INTERACTION_NOT_READY: &str = "INTERACTION_NOT_READY";
/// The interaction was already answered or cancelled.
pub const INTERACTION_CLOSED: &str = "INTERACTION_CLOSED";
/// A decision the server did not advertise for this prompt.
pub const INTERACTION_INVALID_DECISION: &str = "INTERACTION_INVALID_DECISION";
/// The action exists but is not available in this state.
pub const ACTION_UNAVAILABLE: &str = "ACTION_UNAVAILABLE";
pub const ASSET_NOT_FOUND: &str = "ASSET_NOT_FOUND";
/// The path was unreadable, or its type or digest did not match.
pub const ASSET_REJECTED: &str = "ASSET_REJECTED";
/// A frame exceeded the negotiated ceiling.
pub const FRAME_TOO_LARGE: &str = "FRAME_TOO_LARGE";
/// Bounded backpressure and the write timeout both ran out; best-effort only,
/// because the transport is already unusable.
pub const CLIENT_TOO_SLOW: &str = "CLIENT_TOO_SLOW";

/// Stable error code: `SCREAMING_SNAKE` (e.g. `CONFIG_INVALID`).
pub trait ErrorCode {
    fn error_code(&self) -> &'static str;
}

/// Presentation level (doc §3 three error states + §4.4 TIMEOUT dual-presentation note).
/// TUI rendering branches on level (page/field-level = highlighted error line,
/// whole-flow-level = full-screen state).
/// **The level is decided by the trigger context, not inferred from the code alone**
/// (e.g. `TIMEOUT` short sync = page-level, long turn = whole-flow-level; §4.4 note
/// and AC table v1.9.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorLevel {
    /// Field-level: only the erroneous object is flagged (e.g. `CONFIG_INVALID` config validation).
    // Short-operation errors (page/field-level) are wired into `UiEvent::Error`
    // when enabled — the current production path only has turn-level errors
    // (`Full`); Field/Page are covered by test fixtures.
    #[allow(dead_code)]
    Field,
    /// Page-level: highlighted error line + retry reachable (short sync read/write timeouts, etc.).
    #[allow(dead_code)]
    Page,
    /// Whole-flow-level: full-screen error state + return path (long-turn failure, auth/permission, etc.).
    Full,
}

/// Error trigger context (#14 contract third dimension, qa #69 / main #71 increment 2):
/// **the presentation level is decided by it, not inferred from `code`** — `TIMEOUT`
/// short sync = page-level, long turn = whole-flow-level.
/// Producers know it when emitting `UiEvent::Error` and carry it explicitly
/// (devex #81 "the level isn't an intrinsic property of the code"; the rendering layer
/// doesn't derive it, tests don't duplicate the mapping table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorContext {
    /// Short synchronous operation (list_models/count_tokens/complete_text) → page-level.
    // Short-operation errors are wired into `UiEvent::Error` when enabled — the
    // current production path only has turn-level (`LongTurn`); ShortSync is covered
    // by test fixtures.
    #[allow(dead_code)]
    ShortSync,
    /// Agent long turn (streaming + multi-turn tools, transport-layer timeout/interrupt) → whole-flow-level.
    LongTurn,
}

/// Debug warning when `GENERIC` is returned explicitly (reminder to register).
/// Silent in release — `GENERIC`'s semantics are known (no stable code assigned yet),
/// not an accidental loss.
/// Callers: explicit `GENERIC` return paths + boxed exit (types missing from the macro
/// registry automatically fall to `GENERIC`) branches (debug build); in release the
/// function is cfg'd out entirely, so no calls are expected.
#[cfg(debug_assertions)]
pub fn missing_code<T: std::fmt::Debug + ?Sized>(err: &T) {
    eprintln!("[bingo] error: {err:?} uses GENERIC (missing stable error code)");
}

/// Single exit mapping: both GUI (TUI) and CLI exits must get their code through this
/// function; no per-exit implementations.
pub fn map_error<E: ErrorCode + ?Sized>(err: &E) -> &'static str {
    err.error_code()
}

/// msg escaping for the non-TTY error contract (AC-31/32): newlines/tabs/CRs normalize
/// to spaces, the main msg truncates at 200 chars — single-line stability, protects
/// `key=value` parsing.
/// Multi-line stacks go separately in `detail=` (`--verbose`); this function only
/// handles the main `msg` field.
pub fn sanitize_msg(msg: &str) -> String {
    let normalized: String = msg
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' || c == '\r' {
                ' '
            } else {
                c
            }
        })
        .collect();
    normalized.chars().take(200).collect()
}

/// Allowlist for explicit `GENERIC` (exemption table for the drift-guard test).
/// Entries use locatable paths (e.g. `"tool::bash::Error::NonZeroExit"`); each must
/// carry a `TODO(generic-allow): <issue>/<date> <reason>` comment.
/// Only read by the drift-guard test (the release surface is the contract file itself);
/// no references outside test builds are expected.
#[cfg_attr(not(test), allow(dead_code))]
pub const GENERIC_ALLOWLIST: &[&str] = &[];

/// Boxed error (`Box<dyn Error>` at the top level) to a stable code: walk the cause
/// chain for the nearest error implementing [`ErrorCode`]. Unregistered types fall to
/// `GENERIC` (explicit semantics, see [`GENERIC`]).
///
/// Note: the downcast here is just a channel to recover the concrete type from
/// `dyn Error` and call its `error_code()`; all mapping logic lives in each type's own
/// `ErrorCode` implementation, not in the exit — no exit-side mapping match by type name.
pub fn error_code_boxed(err: &(dyn std::error::Error + 'static)) -> &'static str {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(code) = downcast_error_code(e) {
            return code;
        }
        cur = e.source();
    }
    // Types missing from the macro registry (or not implementing ErrorCode) fall to
    // GENERIC: warned under debug (v1.14 requirement), semantics known in release
    // (no stable code assigned yet).
    #[cfg(debug_assertions)]
    missing_code(err);
    GENERIC
}

/// Downcast `&dyn Error` to the concrete type implementing `ErrorCode` and take the code.
/// The macro lists all known types: register new error types implementing `ErrorCode` here.
macro_rules! downcast_error_code {
    ($err:expr, $($t:ty),+ $(,)?) => {{
        let mut found: Option<&'static str> = None;
        $(
            if found.is_none()
                && let Some(e) = $err.downcast_ref::<$t>()
            {
                found = Some(<$t as $crate::error::ErrorCode>::error_code(e));
            }
        )+
        found
    }};
}

fn downcast_error_code(err: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    downcast_error_code!(
        err,
        crate::api::client::ClientError,
        crate::query::QueryError,
        crate::tool::ToolError,
        crate::settings::SettingsError,
        crate::team::TeamError,
        crate::tasks::TaskError,
        crate::transcript::TranscriptError,
        crate::experience::ExperienceError,
        crate::hooks::HookError,
        crate::mcp::McpError,
        crate::share::ShareError,
        crate::storage::StorageError,
        crate::update::UpdateError,
        crate::app_server::AppServerError,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stable codes must be SCREAMING_SNAKE (AC-35).
    fn assert_screaming_snake(code: &str) {
        assert!(
            !code.is_empty()
                && code.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "error code {code:?} must match ^[A-Z][A-Z0-9_]*$"
        );
    }

    /// Variants with explicit GENERIC must be registered in GENERIC_ALLOWLIST.
    fn is_allowed_generic(path: &str) -> bool {
        GENERIC_ALLOWLIST.contains(&path)
    }

    /// Assert every variant of every module maps to a non-GENERIC stable code
    /// (AC-40/41/43): no variant may fall to GENERIC without an explicit
    /// GENERIC_ALLOWLIST entry.
    fn assert_stable_codes<'a, T: ErrorCode + std::fmt::Debug + 'a>(
        path: &str,
        variants: impl IntoIterator<Item = &'a T>,
    ) {
        for variant in variants {
            let code = variant.error_code();
            assert_screaming_snake(code);
            assert!(
                is_allowed_generic(path) || code != GENERIC,
                "{path}: {variant:?} falls to GENERIC without an allowlist entry"
            );
        }
    }

    #[test]
    fn client_error_codes() {
        use crate::api::client::ClientError;
        let variants = vec![
            ClientError::MissingApiKey,
            ClientError::InvalidApiKey("k".into()),
            ClientError::Api {
                status: 401,
                body: String::new(),
            },
            ClientError::Api {
                status: 403,
                body: String::new(),
            },
            ClientError::Api {
                status: 429,
                body: String::new(),
            },
            ClientError::Api {
                status: 500,
                body: String::new(),
            },
            ClientError::ContextOverflow {
                status: 400,
                body: String::new(),
            },
            ClientError::Stream("s".into()),
            ClientError::Timeout,
            ClientError::Unsupported("count_tokens".into()),
            ClientError::Config("unknown protocol".into()),
        ];
        assert_stable_codes("api::client::ClientError", &variants);
        assert_eq!(ClientError::Timeout.error_code(), "TIMEOUT");
        assert_eq!(ClientError::MissingApiKey.error_code(), "AUTH_REQUIRED");
        assert_eq!(
            ClientError::Config("unknown protocol".into()).error_code(),
            "CONFIG_INVALID"
        );
        let denied = ClientError::Api {
            status: 403,
            body: String::new(),
        };
        assert_eq!(denied.error_code(), "PERMISSION_DENIED");
        let server = ClientError::Api {
            status: 500,
            body: String::new(),
        };
        assert_eq!(server.error_code(), "SERVER_ERROR");
        let overflow = ClientError::ContextOverflow {
            status: 400,
            body: String::new(),
        };
        assert_eq!(overflow.error_code(), "CONTEXT_OVERFLOW");
        // The `ClientError::Transport` variant can't be constructed at runtime
        // (reqwest::Error has no public constructor API, all pub(crate) in 0.13.x):
        // the mapping is locked down by `transport_offline_code` and asserted here
        // (same source as the drift-guard test's other variants).
        assert_eq!(crate::api::client::transport_offline_code(), "OFFLINE");
    }

    #[test]
    fn query_error_forwards_client_and_tool() {
        use crate::api::client::ClientError;
        use crate::query::QueryError;
        assert_eq!(
            QueryError::Protocol("p".into()).error_code(),
            "SERVER_ERROR"
        );
        assert_eq!(
            QueryError::Client(ClientError::Timeout).error_code(),
            "TIMEOUT"
        );
        assert_eq!(
            QueryError::Tool(crate::tool::ToolError::failed("x")).error_code(),
            "TOOL_FAILED"
        );
    }

    #[test]
    fn config_and_storage_errors() {
        use crate::settings::SettingsError;
        assert_eq!(
            SettingsError::Io(std::io::Error::other("x")).error_code(),
            "CONFIG_INVALID"
        );
        assert_eq!(
            SettingsError::Parse {
                path: std::path::PathBuf::from("settings.json"),
                source: serde_json::from_str::<()>("x").unwrap_err(),
            }
            .error_code(),
            "CONFIG_INVALID"
        );
        // TeamError's 3 variants explicitly enumerated (guardrail 5: assert per variant).
        use crate::team::TeamError;
        let team_variants = vec![
            TeamError::Invalid("x".into()),
            TeamError::Io(std::io::Error::other("x")),
            TeamError::Parse(serde_json::from_str::<()>("x").unwrap_err()),
        ];
        assert_stable_codes("team::TeamError", &team_variants);
        for v in &team_variants {
            assert_eq!(
                v.error_code(),
                "CONFIG_INVALID",
                "every TeamError variant falls to a config error"
            );
        }
        // TaskError's 5 variants explicitly enumerated (AC-40).
        use crate::tasks::TaskError;
        let task_variants = vec![
            TaskError::Io(std::io::Error::other("x")),
            TaskError::InvalidId("x".into()),
            TaskError::Serialize(serde_json::from_str::<()>("x").unwrap_err()),
            TaskError::CreateConflict("1".into()),
            TaskError::Parse {
                path: "p".into(),
                detail: "d".into(),
            },
        ];
        assert_stable_codes("tasks::TaskError", &task_variants);
        for v in &task_variants {
            assert_eq!(
                v.error_code(),
                "STORAGE_ERROR",
                "every TaskError variant falls to a storage error"
            );
        }
        use crate::transcript::TranscriptError;
        assert_eq!(
            TranscriptError::Io(std::io::Error::other("x")).error_code(),
            "STORAGE_ERROR"
        );
        assert_eq!(
            TranscriptError::Parse(serde_json::from_str::<()>("x").unwrap_err()).error_code(),
            "STORAGE_ERROR"
        );
        // StorageError's 2 variants explicitly enumerated (AC-40).
        use crate::storage::StorageError;
        let storage_variants = vec![
            StorageError::HomeUnavailable,
            StorageError::Io {
                operation: "read",
                path: std::path::PathBuf::from("p"),
                source: std::io::Error::other("x"),
            },
        ];
        assert_stable_codes("storage::StorageError", &storage_variants);
        assert_eq!(storage_variants[0].error_code(), "CONFIG_INVALID");
        assert_eq!(storage_variants[1].error_code(), "STORAGE_ERROR");
        // All 6 ShareError variants explicitly enumerated (AC-40).
        use crate::share::ShareError;
        let share_variants = vec![
            ShareError::Io(std::io::Error::other("x")),
            ShareError::Json(serde_json::from_str::<()>("x").unwrap_err()),
            ShareError::Transcript(TranscriptError::Io(std::io::Error::other("x"))),
            ShareError::NoSessions,
            ShareError::SessionNotFound("x".into()),
            ShareError::Upload("boom".into()),
        ];
        assert_stable_codes("share::ShareError", &share_variants);
        for v in &share_variants {
            assert_eq!(
                v.error_code(),
                "STORAGE_ERROR",
                "every ShareError variant falls to a storage error"
            );
        }
        use crate::experience::ExperienceError;
        assert_eq!(
            ExperienceError::Io(std::io::Error::other("x")).error_code(),
            "STORAGE_ERROR"
        );
        // All UpdateError variants explicitly enumerated (Network has no public constructor; the
        // `update::network_error_code` mapping pins it, same pattern as ClientError::Transport).
        use crate::update::UpdateError;
        let update_variants = vec![
            UpdateError::Http { status: 503 },
            UpdateError::BadResponse("x".into()),
            UpdateError::ChecksumsUnavailable("x".into()),
            UpdateError::ChecksumMismatch {
                expected: "e".into(),
                got: "g".into(),
            },
            UpdateError::ArchiveInvalid("x".into()),
            UpdateError::MissingBinary("bingo".into()),
            UpdateError::UnsupportedPlatform("linux/aarch64".into()),
            UpdateError::Io(std::io::Error::other("x")),
        ];
        assert_stable_codes("update::UpdateError", &update_variants);
        assert_eq!(UpdateError::Http { status: 503 }.error_code(), "OFFLINE");
        assert_eq!(
            UpdateError::BadResponse("x".into()).error_code(),
            "SERVER_ERROR"
        );
        assert_eq!(
            UpdateError::ChecksumsUnavailable("x".into()).error_code(),
            "CHECKSUMS_UNAVAILABLE"
        );
        assert_eq!(
            UpdateError::ChecksumMismatch {
                expected: "e".into(),
                got: "g".into()
            }
            .error_code(),
            "CHECKSUM_MISMATCH"
        );
        assert_eq!(
            UpdateError::ArchiveInvalid("x".into()).error_code(),
            "ARCHIVE_INVALID"
        );
        assert_eq!(
            UpdateError::MissingBinary("bingo".into()).error_code(),
            "ARCHIVE_INVALID"
        );
        assert_eq!(
            UpdateError::UnsupportedPlatform("linux/aarch64".into()).error_code(),
            "UNSUPPORTED_PLATFORM"
        );
        assert_eq!(
            UpdateError::Io(std::io::Error::other("x")).error_code(),
            "STORAGE_ERROR"
        );
        assert_eq!(crate::update::network_error_code(), "OFFLINE");
    }

    #[test]
    fn hook_and_mcp_and_tool() {
        use crate::hooks::HookError;
        assert_eq!(HookError::Failed("x".into()).error_code(), "HOOK_FAILED");
        use crate::mcp::McpError;
        assert_eq!(
            McpError::Connect {
                server: "s".into(),
                detail: "d".into()
            }
            .error_code(),
            "SERVER_ERROR"
        );
        use crate::tool::ToolError;
        assert_eq!(ToolError::failed("x").error_code(), "TOOL_FAILED");
    }

    #[test]
    fn boxed_error_walks_cause_chain() {
        use crate::api::client::ClientError;
        use crate::query::QueryError;
        let q = QueryError::Client(ClientError::Timeout);
        assert_eq!(error_code_boxed(&q), "TIMEOUT");
        // Unregistered types fall to GENERIC.
        let unknown: Box<dyn std::error::Error> = std::io::Error::other("x").into();
        assert_eq!(error_code_boxed(&*unknown), GENERIC);
    }

    /// The macro registry covers all ErrorCode-implementing types (guardrail 4
    /// "registry is the contract's second place"): each of the 13 registered types is
    /// asserted non-GENERIC through the boxed exit — a type implementing ErrorCode that
    /// only takes effect on the TUI exit while missing from the downcast macro would
    /// silently fall to GENERIC on the CLI exit and turn this test red. Cross-checked
    /// against the `downcast_error_code` macro list (double registration; CI red if either is missing).
    #[test]
    fn boxed_export_covers_all_registered_modules() {
        use crate::api::client::ClientError;
        use crate::experience::ExperienceError;
        use crate::hooks::HookError;
        use crate::mcp::McpError;
        use crate::query::QueryError;
        use crate::settings::SettingsError;
        use crate::share::ShareError;
        use crate::storage::StorageError;
        use crate::tasks::TaskError;
        use crate::team::TeamError;
        use crate::tool::ToolError;
        use crate::transcript::TranscriptError;
        use crate::update::UpdateError;
        let samples: Vec<Box<dyn std::error::Error>> = vec![
            Box::new(ClientError::MissingApiKey),
            Box::new(QueryError::Protocol("p".into())),
            Box::new(ToolError::failed("x")),
            Box::new(SettingsError::Io(std::io::Error::other("x"))),
            Box::new(TeamError::Invalid("x".into())),
            Box::new(TaskError::InvalidId("x".into())),
            Box::new(TranscriptError::Io(std::io::Error::other("x"))),
            Box::new(ExperienceError::Io(std::io::Error::other("x"))),
            Box::new(HookError::Failed("x".into())),
            Box::new(McpError::Connect {
                server: "s".into(),
                detail: "d".into(),
            }),
            Box::new(ShareError::SessionNotFound("x".into())),
            Box::new(StorageError::HomeUnavailable),
            Box::new(UpdateError::Http { status: 503 }),
            Box::new(crate::app_server::AppServerError::ServeUnavailable),
        ];
        assert_eq!(
            samples.len(),
            14,
            "the boxed exit should have 14 registered types: new ErrorCode implementors must be \
             `downcast_error_code` macro registration + an instance in this test; missing either turns CI red"
        );
        for e in &samples {
            let code = error_code_boxed(e.as_ref());
            assert!(
                code != GENERIC,
                "boxed exit falls to GENERIC: {e:?} is not in the downcast macro registry"
            );
            assert_screaming_snake(code);
        }
    }

    #[test]
    fn sanitize_msg_normalizes_and_truncates() {
        // AC-31: newlines/tabs/CRs normalize to spaces; the single line isn't broken.
        assert_eq!(
            crate::error::sanitize_msg("line1\nline2\tline3\rline4"),
            "line1 line2 line3 line4"
        );
        // AC-32: truncate at 200 chars (character count, not bytes; any multibyte char counts once).
        let long = "❤".repeat(300);
        assert_eq!(crate::error::sanitize_msg(&long).chars().count(), 200);
        let ascii = "x".repeat(250);
        assert_eq!(crate::error::sanitize_msg(&ascii).len(), 200);
    }
}
