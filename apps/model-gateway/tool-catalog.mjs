import { estimateTokens } from './context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from './tool-protocols.mjs';
import { toolFailure } from './tool-paths.mjs';

const discoveryNames = new Set(['tool.search', 'tool.load', 'tool.result.read']);

function querySignals(message) {
  const text = message.toLowerCase(), terms = new Set(text.match(/[a-z0-9_-]{3,}/g) ?? []);
  for (const phrase of text.match(/\p{Script=Han}{2,}/gu) ?? []) {
    if (terms.size >= 64) break;
    terms.add(phrase);
    const characters = Array.from(phrase);
    for (let index = 0; index < characters.length - 1 && terms.size < 64; index++)
      terms.add(characters[index] + characters[index + 1]);
  }
  return { terms: [...terms].slice(0, 64),
    web: /最新|最近|当前|现在|今天|今日|新闻|官网|官方|上线|发布|搜索|查询|查证|联网|搜一下|是谁|什么时候|什么时间|多少钱/.test(text) ||
      /\b(?:latest|current|recent|today|news|official|released?|announced?|search|who|when|price|weather)\b|\blook\s+up\b/.test(text),
    docs: /文档|接口|代码|编程|开发|框架|库的|库怎么/.test(text) ||
      /\b(?:api|sdk|docs?|documentation|library|libraries|framework|programming|code|typescript|python|dotnet|winui|react)\b/.test(text),
    url: /https?:\/\/\S+/i.test(text) };
}

function relevanceScore(tool, signals) {
  const name = tool.name.toLowerCase(), description = String(tool.description ?? '').toLowerCase();
  let score = signals.terms.reduce((sum, word) => sum + (name.includes(word) ? 3 : description.includes(word) ? 1 : 0), 0);
  const docs = /context7|query[-_]docs|resolve[-_]library[-_]id|(?:search|fetch|get)[-_](?:docs|documentation)/.test(name);
  const webSearch = /web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search/.test(name) || /public web search|search (?:the )?(?:web|internet)/.test(description);
  const search = /(?:^|[._-])search(?:$|[._-])/.test(name);
  const fetch = /(?:^|[._-])(?:fetch|web_fetch|fetch_url)(?:$|[._-])/.test(name);
  if (signals.docs && docs) score += 28;
  if (signals.web || signals.docs) score += webSearch ? (signals.docs ? 12 : 24) : search ? 6 : 0;
  if (fetch && (signals.web || signals.docs || signals.url)) score += signals.url ? 32 : 10;
  return score;
}

/** Discovery can be large; only a bounded, explicitly selected projection enters a model request. */
export class ModelToolCatalog {
  constructor(descriptors, { protocol, tokenBudget = 16000, message = '' } = {}) {
    this.descriptors = descriptors;
    this.protocol = protocol;
    this.tokenBudget = Math.max(0, Math.floor(tokenBudget));
    this.selected = [];
    const signals = querySignals(message), scores = new Map(descriptors.map(tool => [tool, relevanceScore(tool, signals)]));
    const ordered = [...descriptors].sort((left, right) =>
      Number(discoveryNames.has(right.name)) - Number(discoveryNames.has(left.name)) ||
      Number(right.source === 'builtin') - Number(left.source === 'builtin') || scores.get(right) - scores.get(left) ||
      left.name.localeCompare(right.name));
    for (const descriptor of ordered) if (this.fits([...this.selected, descriptor])) this.selected.push(descriptor);
  }

  fits(descriptors) {
    return descriptors.length <= MAX_MODEL_TOOLS &&
      estimateTokens(JSON.stringify(toolDeclarations(this.protocol, wireCatalog(descriptors)))) <= this.tokenBudget;
  }

  wire() { return wireCatalog(this.selected); }

  load(names) {
    const requested = names.map(name => this.descriptors.find(tool => tool.name === name));
    if (requested.some(tool => !tool)) throw toolFailure('工具不存在或已禁用。', 'TOOL_NOT_FOUND', 404);
    const keep = this.selected.filter(tool => tool.source === 'builtin');
    const next = [...keep];
    for (const descriptor of requested) if (!next.some(tool => tool.name === descriptor.name)) next.push(descriptor);
    if (!this.fits(next)) throw toolFailure('请求的工具定义超过本轮模型预算，请减少选择。', 'TOOL_CATALOG_BUDGET', 413);
    for (const descriptor of this.selected) if (!next.some(tool => tool.name === descriptor.name) && this.fits([...next, descriptor])) next.push(descriptor);
    this.selected = next;
    return { loaded: requested.map(tool => tool.name), selectedCount: next.length,
      availableCount: this.descriptors.length, deferredCount: this.descriptors.length - next.length };
  }
}
