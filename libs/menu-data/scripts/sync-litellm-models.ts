#!/usr/bin/env node
// Print recent provider models from BerriAI/litellm so we can refresh the
// curated DIGEST_MODELS list in src/index.ts.
//
// LiteLLM (MIT, https://github.com/BerriAI/litellm) maintains the de-facto
// community catalog of model context/output caps and pricing across every
// major provider. Open-design vendors a filtered slice; here we just want a
// human-readable diff to reviewer eyes when picking which IDs to recommend
// for the digest LLM picker.
//
// Usage:
//   node --experimental-strip-types libs/menu-data/scripts/sync-litellm-models.ts
//
// Then hand-edit the curated lists in libs/menu-data/src/index.ts. We do
// NOT vendor the full ~2000-model JSON because the digest picker needs
// opinionated, per-provider top-3 \u2014 not exhaustive coverage.

const SOURCE_URL =
  'https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json';

const PROVIDER_PREFIXES: Record<string, RegExp[]> = {
  anthropic: [/^claude-/],
  openai: [/^gpt-/, /^o[1-9](-|$)/, /^chatgpt-/],
  gemini: [/^gemini-/],
};

interface LiteLLMEntry {
  mode?: string;
  max_tokens?: number;
  max_output_tokens?: number;
  litellm_provider?: string;
}

async function main() {
  console.log(`fetching ${SOURCE_URL}\n`);
  const res = await fetch(SOURCE_URL);
  if (!res.ok) throw new Error(`fetch ${res.status}: ${res.statusText}`);
  const raw = (await res.json()) as Record<string, unknown>;

  for (const [provider, patterns] of Object.entries(PROVIDER_PREFIXES)) {
    const matches: { id: string; max: number }[] = [];
    for (const [id, value] of Object.entries(raw)) {
      if (id === 'sample_spec' || !value || typeof value !== 'object') continue;
      const entry = value as LiteLLMEntry;
      if (entry.mode !== 'chat') continue;
      if (id.includes('/')) continue; // skip vendored aliases (deepinfra/, gemini/, etc.)
      if (!patterns.some((p) => p.test(id))) continue;
      const max = entry.max_output_tokens ?? entry.max_tokens ?? 0;
      matches.push({ id, max: typeof max === 'number' ? max : 0 });
    }
    matches.sort((a, b) => a.id.localeCompare(b.id));
    console.log(`# ${provider} (${matches.length} chat-mode models)`);
    for (const m of matches) {
      console.log(`  ${m.id.padEnd(45)} max_out=${m.max}`);
    }
    console.log();
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
