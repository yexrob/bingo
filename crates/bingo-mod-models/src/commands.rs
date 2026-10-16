use bingo_sdk::*;
use serde_json::{Value, json};
pub struct Commands(pub crate::Shared);
#[async_trait::async_trait]
impl Interceptor for Commands {
    fn id(&self) -> &str {
        "models.commands"
    }
    fn matcher(&self) -> LinkMatcher {
        LinkMatcher::at(catalogue::COMMAND_RUN)
    }
    async fn chain(
        &self,
        _: &catalogue::Entry,
        cx: &HookContext,
        value: Value,
        next: Next<'_, Value, Value>,
    ) -> Result<Value, KernelError> {
        let command: catalogue::CommandRun =
            serde_json::from_value(value.clone()).map_err(crate::error)?;
        if command.surface != Some(render::SurfaceName::Terminal) {
            return next.run(value).await;
        }
        let name = value["command"].as_str().unwrap_or_default();
        let args = value["args"].as_str().unwrap_or_default().trim();
        if !opens(name, args, &cx.host).await? {
            return next.run(value).await;
        }
        let fallback = fallback(args, &value, next).await;
        let (picker, providers) = opening(name, args, cx).await?;
        self.0
            .lock()
            .map_err(crate::error)?
            .insert(cx.session.clone(), picker.clone());
        crate::state::publish(&cx.host, &cx.session, &picker).await?;
        probe(&cx.host, &cx.session, &providers);
        Ok(
            json!({"kind":"surface","action":{"name":"Pane","args":{"plugin":"bingo.mod.models","id":picker.id}},"fallback":fallback}),
        )
    }
}
/// Ask every provider not yet heard from, all at once and after the picker is up: its
/// rows answer as each probe does, and the command never waits on an endpoint.
fn probe(host: &HostHandle, session: &SessionId, providers: &[CatalogEntry]) {
    let refreshes: Vec<String> = providers
        .iter()
        .filter(|p| crate::label::reach(p) == crate::label::Reach::Unchecked)
        .map(|p| format!("refresh {}", p.id))
        .collect();
    if refreshes.is_empty() {
        return;
    }
    let (host, session) = (host.clone(), session.clone());
    tokio::spawn(async move {
        futures::future::join_all(
            refreshes
                .iter()
                .map(|args| crate::bridge::run(&host, &session, "models", args, None)),
        )
        .await;
    });
}
async fn opens(name: &str, args: &str, host: &HostHandle) -> Result<bool, KernelError> {
    match name {
        "think" | "models" | "provider" => Ok(args.is_empty()),
        "login" | "logout" => Ok(args.is_empty()),
        "model" if args.is_empty() => Ok(true),
        "model" if filters(args) => {
            let models = host.catalog(CatalogKind::Models).await?.entries;
            Ok(!models
                .iter()
                .any(|m| m.id == args || m.id.split_once('/').is_some_and(|(_, id)| id == args)))
        }
        _ => Ok(false),
    }
}
/// A `provider/model` argument names a model outright, listed or not, so it
/// is the kernel's to switch. A bare word or a `provider/` with nothing after
/// it names no model and filters the list.
fn filters(args: &str) -> bool {
    if args.contains(char::is_whitespace) || args == "next" {
        return false;
    }
    match args.split_once('/') {
        None => true,
        Some((provider, model)) => !provider.is_empty() && model.is_empty(),
    }
}

async fn fallback(args: &str, value: &Value, next: Next<'_, Value, Value>) -> String {
    if args.is_empty() {
        let plain = next
            .run(value.clone())
            .await
            .unwrap_or_else(|error| json!({"text":error.message}));
        serde_json::from_value::<catalogue::CommandReply>(plain)
            .ok()
            .map(CommandOutcome::from)
            .and_then(|outcome| match outcome {
                CommandOutcome::View { view } => Some(view.fold()),
                CommandOutcome::Applied { message } => message,
                _ => None,
            })
    } else {
        None
    }
    .unwrap_or_else(|| "Choose with /model <provider/model>; /model next cycles pins.".into())
}
async fn opening(
    name: &str,
    args: &str,
    cx: &HookContext,
) -> Result<(crate::state::Picker, Vec<CatalogEntry>), KernelError> {
    let attached = crate::bridge::attach(&cx.host, &cx.session).await?;
    let mut picker = crate::state::Picker {
        id: IntentId::mint().to_string(),
        all: name == "models",
        props: component::ModelPickerProps {
            filter: if name == "model" {
                args.into()
            } else {
                String::new()
            },
            effort: serde_json::from_value(attached.snapshot.config.kernel["thinking"].clone())
                .unwrap_or_default(),
            ..Default::default()
        },
        ..Default::default()
    };
    let models = cx.host.catalog(CatalogKind::Models).await?.entries;
    let providers = cx.host.catalog(CatalogKind::Providers).await?.entries;
    let rows = crate::rows::rows(&models, &providers, &picker.props.filter, picker.all);
    let current = crate::state::chat(&attached.snapshot).current;
    picker.props.selected = rows
        .iter()
        .find(|r| r.model() == Some(current.as_str()))
        .map(crate::rows::Row::key);
    crate::keys::keep_cursor(&mut picker, &rows);
    start_provider(&mut picker, &providers, &current, name);
    Ok((picker, providers))
}

fn start_provider(
    picker: &mut crate::state::Picker,
    providers: &[CatalogEntry],
    current: &str,
    name: &str,
) {
    if !matches!(name, "login" | "logout") {
        return;
    }
    let provider = providers
        .iter()
        .find(|p| name == "login" && crate::label::reach(p) == crate::label::Reach::Missing)
        .or_else(|| {
            providers
                .iter()
                .find(|p| current.split_once('/').is_some_and(|(id, _)| p.id == id))
        })
        .or_else(|| providers.first());
    let Some(provider) = provider else {
        return;
    };
    if name == "login" {
        picker.props.selected = Some(crate::rows::Row::Provider(provider.id.clone()).key());
    } else {
        picker.sheet = Some(crate::sheet::Sheet::Manage {
            provider: provider.id.clone(),
            selected: 0,
            pending: None,
            confirm: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn model_aliases_open_the_picker_and_keep_each_original_fallback() {
        for name in ["models", "provider"] {
            let (host, command) = crate::test_support::host();
            let (models, providers) = crate::test_support::entries();
            host.on_catalog(move |kind| {
                let entries = if kind == CatalogKind::Models {
                    models.clone()
                } else {
                    providers.clone()
                };
                async move { Ok(Catalog { kind, entries }) }
            });
            let cx = HookContext {
                session: command.session,
                cwd: command.cwd,
                host: command.host,
                item: None,
                turn: None,
                provider: None,
                model: None,
            };
            let shared = crate::Shared::default();
            let opener = Commands(shared.clone());
            let input = json!({"command":name,"args":"","surface":"terminal"});
            let expected = input.clone();
            let reply = opener
                .chain(
                    catalogue::COMMAND_RUN,
                    &cx,
                    input,
                    Next::new(move |input| {
                        assert_eq!(input, expected);
                        Box::pin(async move {
                            if name == "provider" {
                                Err(KernelError::new(ErrorCode::InvalidInput, "provider usage"))
                            } else {
                                Ok(json!(CommandOutcome::View {
                                    view: View::text("all models")
                                }))
                            }
                        })
                    }),
                )
                .await
                .unwrap();
            assert_eq!(reply["action"]["name"], "Pane");
            assert_eq!(reply["action"]["args"]["plugin"], "bingo.mod.models");
            assert_eq!(
                reply["fallback"],
                if name == "models" {
                    "all models"
                } else {
                    "provider usage"
                }
            );
            assert_eq!(shared.lock().unwrap()[&cx.session].all, name == "models");
        }
    }

    #[tokio::test]
    async fn alias_arguments_and_other_surfaces_are_passed_through_unchanged() {
        let (_, command) = crate::test_support::host();
        let cx = HookContext {
            session: command.session,
            cwd: command.cwd,
            host: command.host,
            item: None,
            turn: None,
            provider: None,
            model: None,
        };
        for (name, args, surface) in [
            ("models", "refresh p", Some("terminal")),
            ("provider", "remove p", Some("terminal")),
            ("models", "", Some("text")),
            ("provider", "", Some("chat")),
            ("provider", "", None),
        ] {
            let input = json!({"command":name,"args":args,"surface":surface});
            let expected = input.clone();
            let reply = Commands(crate::Shared::default())
                .chain(
                    catalogue::COMMAND_RUN,
                    &cx,
                    input,
                    Next::new(move |input| {
                        assert_eq!(input, expected);
                        Box::pin(async { Ok(json!({"text":"original command"})) })
                    }),
                )
                .await
                .unwrap();
            assert_eq!(reply, json!({"text":"original command"}));
        }
    }

    #[test]
    fn a_bare_word_or_a_provider_prefix_filters_and_a_path_switches() {
        assert!(filters("opus"));
        assert!(!filters("fake/unlisted"));
        assert!(!filters("next"));
        assert!(!filters("pin fake/a"));
        assert!(filters("openai/"), "a provider prefix filters");
        assert!(!filters("/"));
        assert!(!filters("/gpt-5"));
    }
    #[tokio::test]
    async fn login_opens_on_an_unsigned_provider_and_one_escape_closes_it() {
        let (host, command) = crate::test_support::host();
        let (models, mut providers) = crate::test_support::entries();
        providers.push(CatalogEntry {
            id: "unsigned".into(),
            label: "unsigned".into(),
            meta: json!({"auth":{"kind":"missing"}}),
        });
        host.on_catalog(move |kind| {
            let entries = if kind == CatalogKind::Models {
                models.clone()
            } else {
                providers.clone()
            };
            async move { Ok(Catalog { kind, entries }) }
        });
        let cx = HookContext {
            session: command.session.clone(),
            cwd: command.cwd.clone(),
            host: command.host.clone(),
            item: None,
            turn: None,
            provider: None,
            model: None,
        };
        let (picker, _) = opening("login", "", &cx).await.unwrap();
        assert!(picker.sheet.is_none(), "login starts at the provider list");
        assert_eq!(picker.props.selected.as_deref(), Some("provider:unsigned"));
        let shared = crate::Shared::default();
        shared
            .lock()
            .unwrap()
            .insert(command.session.clone(), picker.clone());
        crate::keys::Handler(shared.clone())
            .run(
                &json!({"handler":picker.id,"event":"press","value":{"key":"esc"}}).to_string(),
                &command,
            )
            .await
            .unwrap();
        assert!(shared.lock().unwrap().is_empty());
        assert!(host.calls().iter().any(|call| matches!(call,
            testing::HostCall::Signal { kind, payload, .. } if kind == "picker" && payload.is_null()
        )));
    }
}
