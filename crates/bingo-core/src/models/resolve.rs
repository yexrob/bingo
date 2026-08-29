//! One pure function turns four owners into the capabilities a turn reads.

use bingo_sdk::{EndpointCapabilities, ModelCapabilities};

use super::catalog::ModelFacts;
use super::declared::Declared;

/// The output budget a session asks for unless settings or the model say
/// less: enough for a large file, not so much that it eats the window.
pub const DEFAULT_MAX_TOKENS: u32 = 32_000;

/// A model nobody knows: closed on what a wrong guess would 400 on (output,
/// reasoning), open on what the server corrects (window) or a person sees
/// (images).
const UNKNOWN: ModelFacts = ModelFacts {
    context_window: 200_000,
    max_output: 8_192,
    reasoning: false,
    images: true,
};

/// `declared > learned clamp > catalogue > unknown`, field by field; the
/// endpoint's facts are composed in, never overridden, because no setting can
/// make a proxy forward an image it strips.
pub fn resolve(
    declared: Option<&Declared>,
    learned: Option<u64>,
    catalogue: Option<ModelFacts>,
    endpoint: EndpointCapabilities,
) -> ModelCapabilities {
    let facts = catalogue.unwrap_or(UNKNOWN);
    let declared = declared.cloned().unwrap_or_default();
    let window = declared
        .context_window
        .unwrap_or_else(|| learned.map_or(facts.context_window, |l| l.min(facts.context_window)));
    ModelCapabilities {
        context_window: window,
        max_output: declared.max_output.unwrap_or(facts.max_output),
        images: declared.images.unwrap_or(facts.images) && endpoint.images,
        reasoning: declared.reasoning.unwrap_or(facts.reasoning),
        count_tokens: endpoint.count_tokens,
        caching: endpoint.caching,
    }
}

/// What a request sends as `max_tokens`: the setting, else the smaller of the
/// model's budget and the default, and never more than half the window so
/// the input side keeps at least half for any declaration.
pub fn max_tokens(capabilities: &ModelCapabilities, declared: Option<u32>) -> u32 {
    let wanted = declared
        .unwrap_or_else(|| capabilities.max_output.min(u64::from(DEFAULT_MAX_TOKENS)) as u32);
    let half = (capabilities.context_window / 2).min(u64::from(u32::MAX)) as u32;
    wanted.min(half).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn endpoint(images: bool) -> EndpointCapabilities {
        EndpointCapabilities {
            images,
            count_tokens: true,
            caching: true,
        }
    }

    fn facts() -> ModelFacts {
        ModelFacts {
            context_window: 1_000_000,
            max_output: 64_000,
            reasoning: true,
            images: true,
        }
    }

    #[test]
    fn an_unknown_model_fails_closed_on_output_and_reasoning() {
        let caps = resolve(None, None, None, endpoint(true));
        assert_eq!(caps.context_window, 200_000);
        assert_eq!(caps.max_output, 8_192);
        assert!(!caps.reasoning);
        assert!(caps.images && caps.count_tokens && caps.caching);
        assert_eq!(max_tokens(&caps, None), 8_192);
    }

    #[test]
    fn the_catalogue_speaks_when_nobody_else_does_and_the_default_budget_bounds_it() {
        let caps = resolve(None, None, Some(facts()), endpoint(true));
        assert_eq!(caps.context_window, 1_000_000);
        assert_eq!(caps.max_output, 64_000);
        assert!(caps.reasoning);
        assert_eq!(max_tokens(&caps, None), DEFAULT_MAX_TOKENS);
        assert_eq!(max_tokens(&caps, Some(64_000)), 64_000);
    }

    #[test]
    fn a_learned_window_clamps_the_catalogue_and_a_declaration_outranks_both() {
        let learned = resolve(None, Some(200_000), Some(facts()), endpoint(true));
        assert_eq!(learned.context_window, 200_000);
        let declared = Declared {
            context_window: Some(500_000),
            reasoning: Some(false),
            ..Declared::default()
        };
        let caps = resolve(
            Some(&declared),
            Some(200_000),
            Some(facts()),
            endpoint(true),
        );
        assert_eq!(caps.context_window, 500_000);
        assert!(!caps.reasoning);
        assert_eq!(caps.max_output, 64_000, "an undeclared field falls through");
    }

    #[test]
    fn images_need_both_the_model_and_the_endpoint() {
        assert!(!resolve(None, None, Some(facts()), endpoint(false)).images);
        let blind = Declared {
            images: Some(false),
            ..Declared::default()
        };
        assert!(!resolve(Some(&blind), None, Some(facts()), endpoint(true)).images);
    }

    fn any_declared() -> impl Strategy<Value = Option<Declared>> {
        proptest::option::of(
            (
                proptest::option::of(8_000u64..2_000_000),
                proptest::option::of(1u64..500_000),
                proptest::option::of(any::<bool>()),
                proptest::option::of(any::<bool>()),
            )
                .prop_map(|(context_window, max_output, reasoning, images)| Declared {
                    context_window,
                    max_output,
                    reasoning,
                    images,
                }),
        )
    }

    fn any_facts() -> impl Strategy<Value = Option<ModelFacts>> {
        proptest::option::of(
            (
                8_000u64..2_000_000,
                1u64..500_000,
                any::<bool>(),
                any::<bool>(),
            )
                .prop_map(|(context_window, max_output, reasoning, images)| {
                    ModelFacts {
                        context_window,
                        max_output,
                        reasoning,
                        images,
                    }
                }),
        )
    }

    proptest! {
        #[test]
        fn the_declared_word_beats_every_other_owner(
            declared in any_declared(),
            learned in proptest::option::of(8_000u64..2_000_000),
            facts in any_facts(),
            endpoint_images in any::<bool>(),
        ) {
            let caps = resolve(declared.as_ref(), learned, facts, endpoint(endpoint_images));
            if let Some(d) = &declared {
                if let Some(w) = d.context_window { prop_assert_eq!(caps.context_window, w); }
                if let Some(o) = d.max_output { prop_assert_eq!(caps.max_output, o); }
                if let Some(r) = d.reasoning { prop_assert_eq!(caps.reasoning, r); }
                if d.images == Some(false) { prop_assert!(!caps.images); }
            }
        }

        #[test]
        fn a_lesson_never_raises_the_window_above_the_catalogue(
            learned in 8_000u64..2_000_000,
            facts in any_facts(),
        ) {
            let caps = resolve(None, Some(learned), facts, endpoint(true));
            let ceiling = facts.map_or(UNKNOWN.context_window, |f| f.context_window);
            prop_assert!(caps.context_window <= ceiling);
            prop_assert!(caps.context_window <= learned);
        }

        #[test]
        fn the_endpoint_can_veto_images_but_never_grant_them(
            declared in any_declared(),
            facts in any_facts(),
            endpoint_images in any::<bool>(),
        ) {
            let caps = resolve(declared.as_ref(), None, facts, endpoint(endpoint_images));
            if !endpoint_images { prop_assert!(!caps.images); }
            if caps.images { prop_assert!(endpoint_images); }
        }

        #[test]
        fn the_input_side_always_keeps_half_the_window(
            declared in any_declared(),
            facts in any_facts(),
            max_tokens_setting in proptest::option::of(1u32..1_000_000),
        ) {
            let caps = resolve(declared.as_ref(), None, facts, endpoint(true));
            let budget = max_tokens(&caps, max_tokens_setting);
            prop_assert!(budget >= 1);
            prop_assert!(u64::from(budget) <= caps.context_window / 2);
            prop_assert!(caps.context_window - u64::from(budget) >= caps.context_window / 2);
        }
    }
}
