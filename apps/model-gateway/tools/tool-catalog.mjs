import { estimateTokens } from '../models/context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { toolFailure } from '../platform/tool-paths.mjs';
import { toolDiscoveryCategory, toolSelectionSignals } from './tool-discovery.mjs';

const discoveryNames = new Set(['tool.search', 'tool.load', 'tool.result.read']);
const coreTool = tool => tool.source === 'builtin' && !tool.name.startsWith('computer.');

function relevanceScore(tool, signals) {
  const name = tool.name.toLowerCase(), description = String(tool.description ?? '').toLowerCase();
  if (name.startsWith('computer.')) return signals.desktop ? 40 : 0;
  if (name.startsWith('terminal.host.')) return signals.hostTerminal ? (name === 'terminal.host.run' ? 40 : 32) : 0;
  let score = signals.terms.reduce((sum, word) => sum + (name.includes(word) ? 3 : description.includes(word) ? 1 : 0), 0);
  const docs = /context7|query[-_]docs|resolve[-_]library[-_]id|(?:search|fetch|get)[-_](?:docs|documentation)/.test(name);
  const webSearch = /web[-_]?search|search[-_]?web|search[-_]?news|news[-_]?search/.test(name) || /public web search|search (?:the )?(?:web|internet)/.test(description);
  const search = /(?:^|[._-])search(?:$|[._-])/.test(name);
  const fetch = /(?:^|[._-])(?:fetch|web_fetch|fetch_url)(?:$|[._-])/.test(name);
  if (signals.browser && toolDiscoveryCategory(tool) === 'browser') score += 32;
  if (signals.retainedNames.has(tool.name)) score += 24;
  if (signals.docs && docs) score += 28;
  if (signals.web || signals.docs) score += webSearch ? (signals.docs ? 12 : 24) : search ? 6 : 0;
  if (fetch && (signals.web || signals.docs || signals.url)) score += signals.url ? 32 : 10;
  return score;
}

/**
 * Discovery can be large; only a bounded, explicitly selected projection enters a model request.
 * 工具发现目录可以很大，但只有明确选定且受预算限制的视图进入模型请求。
 */
export class ModelToolCatalog {
  constructor(descriptors, { protocol, tokenBudget = 16000, message = '', historySignals = [], previousToolNames = [] } = {}) {
    this.descriptors = descriptors.filter(tool => tool.enabled !== false);
    this.canonicalNames = new Map(this.descriptors.flatMap(tool => [[tool.name, tool.name],
      [wireCatalog([tool])[0].wireName, tool.name]]));
    this.protocol = protocol;
    this.tokenBudget = Math.max(0, Math.floor(tokenBudget));
    this.selected = [];
    const signals = toolSelectionSignals(message, { historySignals, previousToolNames });
    const scores = new Map(this.descriptors.map(tool => [tool, relevanceScore(tool, signals)]));
    const ordered = this.descriptors.filter(tool => (!tool.name.startsWith('computer.') || signals.desktop) &&
      (!tool.name.startsWith('terminal.host.') || signals.hostTerminal)).sort((left, right) =>
      Number(discoveryNames.has(right.name)) - Number(discoveryNames.has(left.name)) ||
      (signals.remoteBrowser ? Number(toolDiscoveryCategory(right) === 'browser') - Number(toolDiscoveryCategory(left) === 'browser') : 0) ||
      (signals.desktop ? Number(right.name.startsWith('computer.')) - Number(left.name.startsWith('computer.')) : 0) ||
      Number(coreTool(right)) - Number(coreTool(left)) || scores.get(right) - scores.get(left) ||
      left.name.localeCompare(right.name));
    for (const descriptor of ordered) if (this.fits([...this.selected, descriptor])) this.selected.push(descriptor);
  }

  fits(descriptors) {
    return descriptors.length <= MAX_MODEL_TOOLS &&
      estimateTokens(JSON.stringify(toolDeclarations(this.protocol, wireCatalog(descriptors)))) <= this.tokenBudget;
  }

  wire() { return wireCatalog(this.selected); }

  resolveNames(names) {
    // Exact aliases refer only to enabled descriptors in this request; unknown providers are never guessed or loaded.
    // 精确别名只对应本请求已启用的描述符，未知服务不能通过猜测名称被加载。
    return names.map(name => this.canonicalNames.get(name) ?? name);
  }

  load(names) {
    const resolved = [...new Set(this.resolveNames(names))];
    const requested = resolved.map(name => this.descriptors.find(tool => tool.name === name)).filter(Boolean);
    const unavailable = resolved.filter(name => !this.canonicalNames.has(name)).map(name => ({ name, code: 'TOOL_NOT_FOUND' }));
    if (!requested.length) throw toolFailure('工具不存在或已禁用。', 'TOOL_NOT_FOUND', 404);
    // Explicit discovery may replace ordinary builtin schemas as well as remote ones.
    // Keeping every builtin prevents a small-window model from ever loading the requested capability.
    // 显式发现可以替换普通内置 schema 或远程 schema；小窗口模型若强制保留全部内置工具，将无法装入请求的能力。
    const keep = this.descriptors.filter(tool => discoveryNames.has(tool.name));
    const next = [...keep];
    for (const descriptor of requested) if (!next.some(tool => tool.name === descriptor.name)) next.push(descriptor);
    if (!this.fits(next)) throw toolFailure('请求的工具定义超过本轮模型预算，请减少选择。', 'TOOL_CATALOG_BUDGET', 413);
    for (const descriptor of this.selected) if (!next.some(tool => tool.name === descriptor.name) && this.fits([...next, descriptor])) next.push(descriptor);
    this.selected = next;
    return { loaded: requested.map(tool => tool.name), selectedCount: next.length,
      availableCount: this.descriptors.length, deferredCount: this.descriptors.length - next.length,
      ...(unavailable.length ? { unavailable } : {}) };
  }
}
