use bingo_sdk::*;
use bingo_services::agents_control::{AgentRow, AgentsControl, Message, Named, Session, TeamRow};

#[async_trait::async_trait]
impl AgentsControl for crate::service::AgentService {
    async fn roster(&self, request: Session) -> Result<Vec<AgentRow>, ServiceError> {
        roster(&self.host()?, &request.session)
            .await
            .map_err(failed)
    }
    async fn team(&self, request: Session) -> Result<Vec<TeamRow>, ServiceError> {
        let host = self.host()?;
        let summary = host.summary(&request.session).await.map_err(failed)?;
        crate::team::roster::read(&host, std::path::Path::new(&summary.cwd), &request.session)
            .await
            .map(Option::unwrap_or_default)
            .map_err(failed)
    }
    async fn stop(&self, request: Named) -> Result<String, ServiceError> {
        crate::stop::stop(&self.host()?, &request.session, &request.name)
            .await
            .map_err(failed)
    }
    async fn dismiss(&self, request: Named) -> Result<String, ServiceError> {
        crate::dismiss::dismiss(&self.host()?, &request.session, &request.name)
            .await
            .map_err(failed)
    }
    async fn message(&self, request: Message) -> Result<String, ServiceError> {
        crate::message::send(
            &self.host()?,
            &request.session,
            &request.name,
            &request.text,
            false,
        )
        .await
        .map_err(failed)
    }
    async fn reopen(&self, request: Message) -> Result<String, ServiceError> {
        crate::message::send(
            &self.host()?,
            &request.session,
            &request.name,
            &request.text,
            true,
        )
        .await
        .map_err(failed)
    }
}
pub async fn roster(host: &HostHandle, session: &SessionId) -> Result<Vec<AgentRow>, KernelError> {
    let mut visible = crate::names::agents(host, session).await?;
    visible.extend(crate::names::siblings(host, session).await?);
    visible.sort_by(|a, b| crate::names::name_of(a).cmp(crate::names::name_of(b)));
    Ok(visible
        .into_iter()
        .map(|summary| AgentRow { summary })
        .collect())
}
fn failed(error: KernelError) -> ServiceError {
    ServiceError(error.message)
}
