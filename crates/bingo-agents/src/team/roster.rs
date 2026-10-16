use bingo_sdk::{HostHandle, KernelError, SessionId};
use bingo_services::agents_control::TeamRow;
use std::path::Path;

pub(crate) async fn read(
    host: &HostHandle,
    cwd: &Path,
    session: &SessionId,
) -> Result<Option<Vec<TeamRow>>, KernelError> {
    let Some(team) = super::file::of(cwd)? else {
        return Ok(None);
    };
    let seated = crate::names::children(host, session).await?;
    Ok(Some(
        team.roles
            .into_iter()
            .map(|role| TeamRow {
                summary: crate::names::named(&seated, &role.name),
                role: role.name,
                agent: role.agent,
            })
            .collect(),
    ))
}
