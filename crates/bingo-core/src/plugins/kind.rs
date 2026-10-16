//! Conservative classification of registered contributions.
#[cfg(test)]
use bingo_sdk::catalogue;
use bingo_sdk::plugin::PluginSwitch;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContributionKind {
    Commands,
    Catalogue,
    Interceptor { request_point: Option<&'static str> },
    Tools,
    Providers,
    Store,
    Surface,
    Service,
    Context,
    Policy,
    EventHooks,
    Lifecycle,
    Hooks,
    Compactor,
    Source,
    Process,
}

pub fn switch_kind(kinds: &[ContributionKind]) -> PluginSwitch {
    kinds
        .iter()
        .find_map(reason)
        .map_or(PluginSwitch::Now, |why| PluginSwitch::NextStart {
            why: why.into(),
        })
}

fn reason(kind: &ContributionKind) -> Option<&'static str> {
    Some(match kind {
        ContributionKind::Commands
        | ContributionKind::Catalogue
        | ContributionKind::EventHooks
        | ContributionKind::Lifecycle
        | ContributionKind::Interceptor {
            request_point: None,
        } => return None,
        ContributionKind::Interceptor {
            request_point: Some(point),
        } => point,
        ContributionKind::Tools => "tools",
        ContributionKind::Providers => "providers",
        ContributionKind::Store => "store",
        ContributionKind::Surface => "surface",
        ContributionKind::Service => "services",
        ContributionKind::Context => "context contributors",
        ContributionKind::Policy => "permission policy",
        ContributionKind::Hooks => "legacy hooks",
        ContributionKind::Compactor => "compactor",
        ContributionKind::Source => "late contributions",
        ContributionKind::Process => "process plugin",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_kind_and_the_first_pinning_contribution() {
        use ContributionKind::*;
        for (kind, why) in [
            (Commands, None),
            (Catalogue, None),
            (
                Interceptor {
                    request_point: None,
                },
                None,
            ),
            (
                Interceptor {
                    request_point: Some("prompt.compose"),
                },
                Some("prompt.compose"),
            ),
            (Tools, Some("tools")),
            (Providers, Some("providers")),
            (Store, Some("store")),
            (Surface, Some("surface")),
            (Service, Some("services")),
            (Context, Some("context contributors")),
            (Policy, Some("permission policy")),
            (EventHooks, None),
            (Lifecycle, None),
            (Hooks, Some("legacy hooks")),
            (Compactor, Some("compactor")),
            (Source, Some("late contributions")),
            (Process, Some("process plugin")),
        ] {
            let expected = why.map_or(PluginSwitch::Now, |why| PluginSwitch::NextStart {
                why: why.into(),
            });
            assert_eq!(switch_kind(&[Commands, kind]), expected, "{kind:?}");
        }
        assert_eq!(
            switch_kind(&[Tools, Store]),
            PluginSwitch::NextStart {
                why: "tools".into()
            }
        );
        assert_eq!(switch_kind(&[]), PluginSwitch::Now);
        for point in [
            catalogue::PROMPT_COMPOSE,
            catalogue::TOOL_DESCRIBE,
            catalogue::TURN_STEP,
        ] {
            assert_eq!(
                switch_kind(&[Interceptor {
                    request_point: point.shapes_request.then_some(point.name.as_ref())
                }]),
                PluginSwitch::NextStart {
                    why: point.name.to_string()
                }
            );
        }
        assert_eq!(
            catalogue::ALL
                .iter()
                .filter(|e| e.shapes_request)
                .map(|e| e.name.as_ref())
                .collect::<Vec<_>>(),
            ["turn.step", "prompt.compose", "tool.describe"]
        );
    }
}

#[cfg(test)]
mod derivation {
    use bingo_sdk::{PluginState, SWITCHED_OFF, plugin::NextStart};
    #[test]
    fn pending_is_only_the_difference_between_wish_and_running() {
        for (wish, now, expected) in [
            (true, true, PluginState::On),
            (
                false,
                false,
                PluginState::Off {
                    why: SWITCHED_OFF.into(),
                },
            ),
            (
                false,
                true,
                PluginState::Pending {
                    now: true,
                    at: NextStart::NextStart,
                },
            ),
            (
                true,
                false,
                PluginState::Pending {
                    now: false,
                    at: NextStart::NextStart,
                },
            ),
        ] {
            for initial in [
                PluginState::On,
                PluginState::Off {
                    why: SWITCHED_OFF.into(),
                },
            ] {
                assert_eq!(super::super::derived(&initial, wish, now), expected);
            }
            for protected in [
                PluginState::Waiting { changed: true },
                PluginState::Waiting { changed: false },
                PluginState::Failed {
                    why: "refused".into(),
                },
                PluginState::Registering,
                PluginState::Off {
                    why: "missing requirement".into(),
                },
            ] {
                assert_eq!(super::super::derived(&protected, wish, now), protected);
            }
        }
    }
}
