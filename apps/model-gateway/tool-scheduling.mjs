const builtinReads = new Set(['filesystem.read', 'filesystem.list', 'filesystem.search', 'filesystem.stat',
  'conversation.history.read', 'conversation.history.search', 'tool.result.read']);
const publicSearches = new Set(['mcp.exa.web_search_exa', 'mcp.brave.brave_web_search', 'mcp.brave-search.brave_web_search']);

/** Scheduling hint only. Every call still passes the broker's ownership, approval and cancellation checks. */
export function canRunInParallel(call) {
  if (!call || typeof call !== 'object' || Array.isArray(call) || typeof call.name !== 'string') return false;
  if (builtinReads.has(call.name)) return true;
  if (publicSearches.has(call.name)) return true;
  if (call.name !== 'mcp.fetch.fetch') return false;
  const url = call.arguments?.arguments?.url;
  if (typeof url !== 'string' || /[\0\r\n]/.test(url)) return false;
  try { return ['http:', 'https:'].includes(new URL(url).protocol); }
  catch { return false; }
}
