import { estimateTokens } from '../models/context.mjs';
import { MAX_MODEL_TOOLS, toolDeclarations, wireCatalog } from '../models/tool-protocols.mjs';
import { toolFailure } from '../platform/tool-paths.mjs';
import { toolDiscoveryCategory, toolSelectionSignals } from './tool-discovery.mjs';

const discoveryNames = new Set(['tool.search', 'tool.load', 'tool.result.read']);
const requiredDiscoveryNames = new Set(['tool.search', 'tool.load']);
const coreTool = tool => tool.source === 'builtin' && tool.modelExposure !== 'on-demand' &&
  !['computer', 'host-terminal'].includes(toolDiscoveryCategory(tool));

function relevanceScore(tool, signals) {
  const name = tool.name.toLowerCase(), description = String(tool.description ?? '').toLowerCase();
  if (name.startsWith('computer.')) return signals.desktop ? 40 : 0;
  if (name.startsWith('terminal.host.')) {
    if (!signals.hostTerminal) return 0;
    return name === 'terminal.host.run' ? (signals.deviceState ? 80 : 40) : 32;
  }
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
 * Discovery can be large; hints rank a bounded candidate projection, with deferred tools still discoverable.
 * 工具发现目录可以很大；线索排序受预算限制的候选视图，延后工具仍可按需发现。
 */
export class ModelToolCatalog {
  constructor(descriptors, { protocol, tokenBudget = 16000, message = '', historySignals = [], previousToolNames = [], taskRelation } = {}) {
    this.descriptors = descriptors.filter(tool => tool.enabled !== false && tool.available !== false);
    this.canonicalNames = new Map(this.descriptors.flatMap(tool => [[tool.name, tool.name],
      [wireCatalog([tool])[0].wireName, tool.name]]));
    this.protocol = protocol;
    this.tokenBudget = Math.max(0, Math.floor(tokenBudget));
    this.selected = [];
    const signals = toolSelectionSignals(message, { historySignals, previousToolNames, taskRelation });
    const scores = new Map(this.descriptors.map(tool => [tool, relevanceScore(tool, signals)]));
    // Hints rank schema candidates; only runtime/configuration facts remove an executable descriptor.
    // Unknown wording must retain a discovery/load path rather than become a semantic prohibition.
    // 线索只排序 schema 候选，只有运行时或配置事实移除可执行描述符；陌生表达保留发现与加载路径，不变成语义禁令。
    const ordered = [...this.descriptors].sort((left, right) =>
      Number(requiredDiscoveryNames.has(right.name)) - Number(requiredDiscoveryNames.has(left.name)) ||
      Number(discoveryNames.has(right.name)) - Number(discoveryNames.has(left.name)) ||
      // A device inspection needs the real host shell before less relevant builtin schemas fill the budget.
      // 本机状态查询先保留真实宿主终端，避免其他内置 schema 先占满预算；执行审批保持原规则。
      (signals.deviceState ? Number(right.name === 'terminal.host.run') - Number(left.name === 'terminal.host.run') : 0) ||
      (signals.hostTerminal ? Number(right.name.startsWith('terminal.host.')) - Number(left.name.startsWith('terminal.host.')) : 0) ||
      (signals.remoteBrowser ? Number(toolDiscoveryCategory(right) === 'browser') - Number(toolDiscoveryCategory(left) === 'browser') : 0) ||
      (signals.desktop ? Number(right.name.startsWith('computer.')) - Number(left.name.startsWith('computer.')) : 0) ||
      // Optional planning/draft interfaces must not displace the actual execution schema in a small window.
      // 可选的规划和草稿接口不能挤占小窗口的实际执行 schema；完整目录与显式加载仍保留它们。
      Number(left.modelExposure === 'on-demand') - Number(right.modelExposure === 'on-demand') ||
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

  _loadableSelection(requestedDescriptors) {
    const requested = [];
    for (const descriptor of requestedDescriptors) {
      const current = this.descriptors.find(tool => tool.name === descriptor?.name);
      if (!current) return null;
      if (!requested.some(tool => tool.name === current.name)) requested.push(current);
    }
    // Result paging can be deferred to fit an explicitly requested action; search/load always stay reachable.
    // Explicitly requested result paging remains mandatory, and its descriptor is never removed from the full catalog.
    // 为容纳明确请求的动作可延后结果分页，搜索/加载始终可达；明确请求的结果分页仍须装入，完整目录不删除其描述符。
    for (const keptNames of [discoveryNames, requiredDiscoveryNames]) {
      const next = this.descriptors.filter(tool => keptNames.has(tool.name));
      for (const descriptor of requested) if (!next.some(tool => tool.name === descriptor.name)) next.push(descriptor);
      if (this.fits(next)) return next;
    }
    return null;
  }

  canLoad(requestedDescriptors) { return this._loadableSelection(requestedDescriptors) !== null; }

  load(names) {
    const resolved = [...new Set(this.resolveNames(names))];
    const requested = resolved.map(name => this.descriptors.find(tool => tool.name === name)).filter(Boolean);
    const unavailable = resolved.filter(name => !this.canonicalNames.has(name)).map(name => ({ name, code: 'TOOL_NOT_FOUND' }));
    if (!requested.length) throw toolFailure('工具不存在或已禁用。', 'TOOL_NOT_FOUND', 404);
    // Explicit discovery may replace ordinary builtin schemas as well as remote ones.
    // Keeping every builtin prevents a small-window model from ever loading the requested capability.
    // 显式发现可以替换普通内置 schema 或远程 schema；小窗口模型若强制保留全部内置工具，将无法装入请求的能力。
    const next = this._loadableSelection(requested);
    if (!next) throw toolFailure('请求的工具定义超过本轮模型预算，请减少选择。', 'TOOL_CATALOG_BUDGET', 413);
    for (const descriptor of this.selected) if (!next.some(tool => tool.name === descriptor.name) && this.fits([...next, descriptor])) next.push(descriptor);
    this.selected = next;
    return { loaded: requested.map(tool => tool.name), selectedCount: next.length,
      availableCount: this.descriptors.length, deferredCount: this.descriptors.length - next.length,
      ...(this.descriptors.some(tool => tool.name === 'tool.result.read') && !next.some(tool => tool.name === 'tool.result.read')
        ? { deferredDiscovery: ['tool.result.read'] } : {}),
      ...(unavailable.length ? { unavailable } : {}) };
  }
}
