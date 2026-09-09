use serde_json::json;

use super::*;

const ADMIN: &str = "ou_admin";
const LISTED: &str = "ou_listed";
const STRANGER: &str = "ou_stranger";

/// One admin, one listed principal, and the same rule for both kinds of chat.
/// The mention gate is off throughout, so what answers is the policy alone.
fn under(policy: Policy) -> Access {
    let rule = Rule {
        policy,
        list: BTreeSet::from([LISTED.to_string()]),
        mention: false,
    };
    Access {
        admins: BTreeSet::from([ADMIN.to_string()]),
        direct: rule.clone(),
        group: rule,
        ..Access::default()
    }
}

fn only(principal: &str) -> BTreeSet<String> {
    BTreeSet::from([principal.to_string()])
}

/// Every policy, for each of the three people it can tell apart, in both
/// kinds of chat — which read the same, because the kind decides which rule
/// applies and nothing else.
#[test]
fn every_policy_admits_the_same_people_wherever_it_is_applied() {
    let table = [
        (Policy::Open, [Ok(()), Ok(()), Ok(())]),
        (Policy::Allowlist, [Ok(()), Ok(()), Err(Refused::NotListed)]),
        (Policy::Blocklist, [Ok(()), Err(Refused::Listed), Ok(())]),
        (
            Policy::Admins,
            [Ok(()), Err(Refused::NotAdmin), Err(Refused::NotAdmin)],
        ),
        (
            Policy::Off,
            [Err(Refused::Off), Err(Refused::Off), Err(Refused::Off)],
        ),
    ];
    for (policy, wanted) in table {
        let access = under(policy);
        for to in [Conversation::direct("oc_1"), Conversation::group("oc_1")] {
            for (principal, want) in [ADMIN, LISTED, STRANGER].into_iter().zip(wanted) {
                assert_eq!(
                    access.admits(&to, principal, true),
                    want,
                    "{policy:?} for {principal} in {to:?}"
                );
            }
        }
    }
}

/// The default is what ran before there was a policy at all (ADR-0016 §4), so
/// no bot goes quiet on upgrade.
#[test]
fn the_default_is_open_with_a_group_engaging_on_a_mention() {
    let access = Access::default();
    let group = Conversation::group("oc_1");
    assert_eq!(access.admits(&group, STRANGER, true), Ok(()));
    assert_eq!(
        access.admits(&group, STRANGER, false),
        Err(Refused::NotAddressed)
    );
    assert_eq!(
        access.admits(&Conversation::direct("oc_1"), STRANGER, false),
        Ok(()),
        "a direct message is addressed by having been sent"
    );
}

#[test]
fn a_rule_that_wants_no_mention_engages_a_group_on_every_message() {
    let access = Access {
        group: Rule {
            mention: false,
            ..Rule::default()
        },
        ..Access::default()
    };
    assert_eq!(
        access.admits(&Conversation::group("oc_1"), STRANGER, false),
        Ok(())
    );
}

/// A blocklisted admin is still an admin; a chat that is off is off for them
/// too, which is the one rule nobody passes.
#[test]
fn an_admin_passes_every_policy_but_off() {
    let with = |policy| Access {
        admins: only(ADMIN),
        direct: Rule {
            policy,
            list: only(ADMIN),
            ..Rule::default()
        },
        ..Access::default()
    };
    let direct = Conversation::direct("oc_1");
    assert_eq!(with(Policy::Blocklist).admits(&direct, ADMIN, true), Ok(()));
    assert_eq!(with(Policy::Admins).admits(&direct, ADMIN, true), Ok(()));
    assert_eq!(
        with(Policy::Off).admits(&direct, ADMIN, true),
        Err(Refused::Off)
    );
}

#[test]
fn a_chats_own_rule_replaces_the_one_for_groups_whole() {
    let access = Access {
        group: Rule {
            policy: Policy::Off,
            ..Rule::default()
        },
        rules: BTreeMap::from([(
            "oc_open".to_string(),
            Rule {
                mention: false,
                ..Rule::default()
            },
        )]),
        ..Access::default()
    };
    assert_eq!(
        access.admits(&Conversation::group("oc_shut"), STRANGER, true),
        Err(Refused::Off)
    );
    assert_eq!(
        access.admits(&Conversation::group("oc_open"), STRANGER, false),
        Ok(()),
        "the whole rule is replaced, mention and all"
    );
    assert_eq!(
        access.admits(&Conversation::direct("oc_shut"), STRANGER, false),
        Ok(()),
        "a group's rule is not a direct chat's"
    );
}

#[test]
fn the_settings_spelling_is_lowercase_words_and_every_field_defaults() {
    let parsed: Access = serde_json::from_value(json!({
        "admins": [ADMIN],
        "group": { "policy": "blocklist", "list": [LISTED], "mention": false },
        "rules": { "oc_1": { "policy": "admins" } },
    }))
    .expect("the policy parses");
    assert_eq!(parsed.group.policy, Policy::Blocklist);
    assert!(!parsed.group.mention);
    assert_eq!(parsed.rules["oc_1"].policy, Policy::Admins);
    assert!(
        parsed.rules["oc_1"].mention,
        "what a rule does not say keeps the default"
    );
    assert_eq!(parsed.direct, Rule::default(), "and so does a whole rule");
    assert_eq!(
        serde_json::from_value::<Access>(json!({})).expect("an empty policy"),
        Access::default()
    );
}
