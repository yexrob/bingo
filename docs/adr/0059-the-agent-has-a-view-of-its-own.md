# 0059 — The agent has a view of its own

## Context

The kernel's identity block (`bingo-core/src/prompt.rs`) tells the model to make the change the person asked for and no more, and to mention adjacent problems rather than fix them unasked. It says nothing about the case where the person's plan is the problem: the model follows a path it can see is wrong, or finds a better one mid-investigation and keeps quiet because the instruction was to do what was asked. The user asked (2026-09-15) that bingo not be an agent that only obeys — that it have a view of its own, raise it first when it thinks the person's approach is wrong or a better one exists, say why, and leave the decision with the person, because the person has not seen what the model has.

That is a stance, not a mechanism: one system block. The kernel's identity is deliberately the minimum every session shares (ADR-0006 keeps it cacheable and plugin-free), and a stance a person may not want belongs where it can be switched off — a plugin (ADR-0057).

## Decision

1. **One plugin, `bingo-persona`**, plugin tier, depending on the sdk and nothing else. It claims one settings key, `persona`, `Replace`, with one field: `text`, the whole block as the person wants it. Absent, the block is the crate's own (§3); present, the person's words replace it entirely — the shipped text is not a template to append to, because a stance is one voice, and two voices in one prompt argue; the empty string is "no block at all" for whoever wants the plugin on and silent. The key lives in the layers like any other, so a project's `.bingo/settings.toml` gives that project its own persona over the person's. Turning the plugin off altogether is `enabledPlugins["bingo.persona"] = false` (ADR-0057 §1), not a second `enabled` here. *(Amended 2026-09-15, user-directed "人格插件可以配置覆盖人格": was "claims no settings key".)*
2. **One contributor, `persona:judgement`**, `Placement::System { order: -20 }`, one cacheable `SystemBlock` whose text is a constant in the crate. It comes after the kernel's identity and before the project's instructions (`context:instructions`, −10) and memory (−5): who bingo is precedes how this project works, so a project's own file can still narrow the stance for that project.
3. **What the block says**, in substance; the crate holds the words. Bingo is a colleague, not an order-taker, and the person has asked for its view. When it thinks the approach it was given is wrong, or finds a better one while working, it says so *before* going on — what it would do instead, why, and what it costs — in a few sentences, without lecturing. Then the person decides: a path held to after hearing the case is taken, and taken well; a decision the work can wait for is waited for; one it cannot is made, said, and explained. It disagrees with a claim, not a person, is as direct against its own view as for it, and says when it is guessing. It never deviates silently: a better path taken without a word is worse than the wrong path taken together. Small choices — a name, an order of steps, a tool — are its own; it raises the ones that change the result, the cost, or what is left afterwards.
4. **The kernel's identity is untouched.** "Make the change the user asked for, no more" stays: the persona changes when bingo speaks up, not what it does after the person has decided.

## Consequences

- New crate `bingo-persona` (+1 member, no dependency of its own). Registered in the binary beside `bingo-context`; listed under features in `ARCHITECTURE.md`.
- The stance costs every request a few hundred cached characters; a person who wants an agent that only obeys switches the plugin off.
- No `/persona` command, no file-path knob, no tone settings: a long text goes in a TOML `"""` string, and the first real ask for a file is the ADR that adds it. An unknown field under `persona` is a startup failure, as `experience`'s is, so a typo does not leave the default in force silently.
- The words are tested for what they reach, not what they say: a black-box run shows the block in the model's request.

## Supersedes

—
