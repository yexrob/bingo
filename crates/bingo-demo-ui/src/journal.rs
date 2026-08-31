//! Where the board lives: the session's own journal, as the extension
//! `bingo.demo.ui`/`board` (ADR-0011 §2). The payload is the *view* a person
//! was shown, and the board reads straight back out of it — nothing is kept
//! between calls, and no shadow record has to be kept in step with the one
//! the journal carries.

use bingo_sdk::{
    ClientIdentity, ErrorCode, HostHandle, KernelError, OpenOptions, SessionId, SessionSelector,
    SessionState, View,
};

use crate::board::Board;

/// The plugin this state belongs to, and the kinds within it.
pub const PLUGIN: &str = "bingo.demo.ui";
/// The journaled kind: back after `--continue`.
pub const BOARD: &str = "board";
/// The live kind: never journaled, gone on a resume (ADR-0013 §2).
pub const PROGRESS: &str = "progress";

/// The board as the journal has it. A session that never published one, or
/// published something this crate cannot read, starts from a fresh board.
pub async fn read(host: &HostHandle, session: &SessionId) -> Result<Board, KernelError> {
    let attachment = host
        .open(
            SessionSelector::ById {
                id: session.clone(),
            },
            ClientIdentity {
                name: "demo-ui".into(),
                surface: "demo-ui".into(),
            },
            OpenOptions::default(),
        )
        .await?;
    Ok(board_of(&attachment.snapshot))
}

fn board_of(snapshot: &SessionState) -> Board {
    snapshot
        .extensions
        .get(PLUGIN)
        .and_then(|kinds| kinds.get(BOARD))
        .and_then(|payload| serde_json::from_value::<View>(payload.clone()).ok())
        .and_then(|view| Board::of_view(&view))
        .unwrap_or_default()
}

/// Publishes the whole board, which is what the kind means: the next snapshot
/// carries exactly this, and so does the next run that continues the session.
pub async fn write(
    host: &HostHandle,
    session: &SessionId,
    board: &Board,
) -> Result<(), KernelError> {
    host.extend(session, PLUGIN, BOARD, view(board)?).await
}

fn view(board: &Board) -> Result<serde_json::Value, KernelError> {
    serde_json::to_value(board.view())
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("a view is json: {e}")))
}
