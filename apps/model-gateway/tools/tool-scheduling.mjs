const builtinReads = new Set(['filesystem.read', 'filesystem.list', 'filesystem.search', 'filesystem.stat', 'web.fetch',
  'conversation.history.read', 'conversation.history.search', 'tool.result.read', 'knowledge.search', 'knowledge.read',
  'knowledge.relations', 'knowledge.experience']);
const publicSearches = new Set(['mcp.exa.web_search_exa', 'mcp.brave.brave_web_search', 'mcp.brave-search.brave_web_search']);

function operationName(name) {
  const official = /^mcp\.official-(exa|fetch|brave-search)(?:-([1-9]\d*))?\.(web_search_exa|fetch|brave_web_search)$/.exec(name);
  if (!official || (official[2] && Number(official[2]) < 2)) return name;
  return `mcp.${official[1]}.${official[3]}`;
}

/**
 * Scheduling hint only. Every call still passes the broker's ownership, approval and cancellation checks.
 * 此处只是调度提示，每次调用仍检查归属、审批和取消。
 */
export function canRunInParallel(call) {
  if (!call || typeof call !== 'object' || Array.isArray(call) || typeof call.name !== 'string') return false;
  if (builtinReads.has(call.name)) return true;
  const name = operationName(call.name);
  if (publicSearches.has(name)) return true;
  if (name !== 'mcp.fetch.fetch') return false;
  const url = call.arguments?.arguments?.url;
  if (typeof url !== 'string' || /[\0\r\n]/.test(url)) return false;
  try { return ['http:', 'https:'].includes(new URL(url).protocol); }
  catch { return false; }
}
