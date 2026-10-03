// Official API documentation snapshot checked 2026-10-04. These are capability
// ceilings, not connection defaults or promises of access for a particular key.
// Exact IDs only: do not infer limits for fine-tunes, proxies or future aliases.
const providers = new Map();

function register(host, model, contextWindowTokens, maxOutputTokens, source, { aliases = [], maxInputTokens } = {}) {
  const models = providers.get(host) ?? new Map();
  const limits = Object.freeze({ contextWindowTokens, ...(maxOutputTokens == null ? {} : { maxOutputTokens }),
    ...(maxInputTokens == null ? {} : { maxInputTokens }), source });
  for (const id of [model, ...aliases]) models.set(id, limits);
  providers.set(host, models);
}

// The published /models example gives current alias metadata. The old
// deepseek-chat/reasoner aliases are intentionally not guessed from their names.
for (const model of ['deepseek-flash', 'deepseek-v4-pro'])
  register('api.deepseek.com', model, 1_048_576, 393_216, 'https://api-docs.deepseek.com/api/list-models/');

register('api.moonshot.cn', 'kimi-k3', 1_048_576, 1_048_576,
  'https://platform.kimi.com/docs/guide/kimi-k3-quickstart');
// K2 docs give a 32K default, not a verified hard output ceiling.
for (const model of ['kimi-k2.7-code', 'kimi-k2.7-code-highspeed', 'kimi-k2.6'])
  register('api.moonshot.cn', model, 262_144, undefined, 'https://platform.kimi.com/docs/pricing/chat');

for (const suffix of ['opus-5-5', 'sonnet-5-5', 'fable-5-1', 'opus-5', 'sonnet-5', 'fable-5',
  'opus-4-8', 'opus-4-7', 'opus-4-6', 'sonnet-4-6'])
  register('api.anthropic.com', `claude-${suffix}`, 1_000_000, 128_000,
    `https://platform.claude.com/docs/en/models/${suffix}/overview`);
for (const [suffix, snapshot] of [['haiku-4-5', '20251001'], ['opus-4-5', '20251101'], ['sonnet-4-5', '20250929']])
  register('api.anthropic.com', `claude-${suffix}`, 200_000, 64_000,
    `https://platform.claude.com/docs/en/models/${suffix}/overview`, { aliases: [`claude-${suffix}-${snapshot}`] });

// Every entry and dated snapshot below was checked against its own model page.
const openai = [
  ['gpt-6-astra', 1_050_000, 128_000, null, 922_000],
  ['gpt-6.1-sol', 1_050_000, 128_000, null, 922_000],
  ['gpt-6-sol', 1_050_000, 128_000, null, 922_000],
  ['gpt-6-luna', 1_050_000, 128_000, null, 922_000],
  ['gpt-5.6-sol', 1_050_000, 128_000, null, 922_000],
  ['gpt-5.6-terra', 1_050_000, 128_000, null, 922_000],
  ['gpt-5.6-luna', 1_050_000, 128_000, null, 922_000],
  ['gpt-5.5', 1_050_000, 128_000, '2026-04-23'],
  ['gpt-5.5-pro', 1_050_000, 128_000, '2026-04-23'],
  ['gpt-5.4', 1_050_000, 128_000, '2026-03-05'],
  ['gpt-5.4-pro', 1_050_000, 128_000, '2026-03-05'],
  ['gpt-5.4-mini', 400_000, 128_000, '2026-03-17', 272_000],
  ['gpt-5.4-nano', 400_000, 128_000, '2026-03-17', 272_000],
  ['gpt-5.3-codex', 400_000, 128_000, null, 272_000],
  ['gpt-5.2', 400_000, 128_000, '2025-12-11'],
  ['gpt-5.2-pro', 400_000, 128_000, '2025-12-11'],
  ['gpt-5.1', 400_000, 128_000, '2025-11-13'],
  ['gpt-5', 400_000, 128_000, '2025-08-07', 272_000],
  ['gpt-5-mini', 400_000, 128_000, '2025-08-07', 272_000],
  ['gpt-5-nano', 400_000, 128_000, '2025-08-07', 272_000],
  ['gpt-5-pro', 400_000, 272_000, '2025-10-06'],
  ['o3', 200_000, 100_000, '2025-04-16'],
  ['o3-pro', 200_000, 100_000, '2025-06-10'],
  ['gpt-4.1', 1_047_576, 32_768, '2025-04-14'],
  ['gpt-4.1-mini', 1_047_576, 32_768, '2025-04-14'],
  ['gpt-4o', 128_000, 16_384, '2024-08-06'],
  ['gpt-4o-mini', 128_000, 16_384, '2024-07-18'],
  ['chat-latest', 400_000, 128_000, null, 272_000],
];
for (const [model, context, output, snapshot, input] of openai)
  register('api.openai.com', model, context, output, `https://developers.openai.com/api/docs/models/${model}`,
    { aliases: snapshot ? [`${model}-${snapshot}`] : [], maxInputTokens: input });

/** Capability lookup never alters stored connections or consults credentials. */
export function resolveModelCapabilities(connection, model) {
  if (typeof connection?.baseUrl !== 'string' || typeof model !== 'string') return {};
  let endpoint;
  try { endpoint = new URL(connection.baseUrl); } catch { return {}; }
  if (endpoint.protocol !== 'https:' || endpoint.port || endpoint.username || endpoint.password || endpoint.search || endpoint.hash)
    return {};
  // Match official API roots only, not a different product mounted on that host.
  if (!['', '/', '/v1', '/v1/'].includes(endpoint.pathname)) return {};
  const capability = providers.get(endpoint.hostname)?.get(model);
  return capability ? { ...capability } : {};
}
