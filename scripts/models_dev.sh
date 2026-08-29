#!/usr/bin/env bash
# Regenerate the embedded models.dev snapshot (ADR-0004).
#
# The kernel reads the raw api.json shape pruned to the fields it uses, so a
# runtime refresh is the same download without the pruning. Providers listed
# here are the ones a bingo provider can be pointed at: the two it speaks to
# directly, and the vendors an OpenAI-compatible proxy commonly fronts (found
# through the cross-provider lookup by model id).
set -euo pipefail
cd "$(dirname "$0")/.."
out=crates/bingo-core/models.dev.json
providers='["anthropic","openai","deepseek","google","xai","mistral","moonshotai","zai","alibaba","minimax","meta","groq"]'
curl -sSL --max-time 120 https://models.dev/api.json \
  | jq --argjson keep "$providers" '
      with_entries(select(.key as $k | $keep | index($k)))
      | with_entries(.value |= {
          models: (.models | with_entries(.value |= {
            limit: {context: .limit.context, output: .limit.output},
            reasoning: .reasoning,
            modalities: {input: .modalities.input}
          }))
        })' > "$out"
printf 'wrote %s: %s providers, %s models, %s bytes\n' "$out" \
  "$(jq 'length' "$out")" "$(jq '[.[] | .models | length] | add' "$out")" "$(wc -c < "$out" | tr -d ' ')"
