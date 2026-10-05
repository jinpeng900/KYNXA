import { randomUUID, createHash } from 'node:crypto';
import { toolFailure } from '../../platform/tool-paths.mjs';
import { inferBrowserTaskIntent } from '../browser-intent-policy.mjs';
import { normalizeWebSources } from './web-source-normalizer.mjs';
export { normalizeWebSources } from './web-source-normalizer.mjs';

const limits = { standard: { queries: 2, pages: 6, durationMs: 45000 }, deep: { queries: 12, pages: 20, durationMs: 180000 } };
const providerKind = descriptor => /(?:^|[._])web_search_exa$/u.test(descriptor.toolName ?? descriptor.name) ? 'exa'
  : /(?:^|[._])brave_web_search$/u.test(descriptor.toolName ?? descriptor.name) ? 'brave' : null;
export const isPublicSearchTool = descriptor => Boolean(providerKind(descriptor));
export const isPublicFetchTool = descriptor => descriptor.name === 'web.fetch' ||
  /^mcp\.(?:official-)?fetch(?:-[1-9]\d*)?\.fetch$/u.test(descriptor.name);

export function isAutomaticBrowserRead(context, descriptor) {
  if ((context.browserTaskIntent ?? inferBrowserTaskIntent(context.message)).explicitBrowserTask) return false;
  return /^mcp\.(?:official-)?(?:chrome-devtools|playwright)(?:-[1-9]\d*)?\.(?:take_snapshot|evaluate_script|browser_snapshot|browser_evaluate)$/u.test(descriptor.name);
}

/** Reuse installed MCP providers through the existing broker; this adapter grants no authority.
 * 复用已安装 MCP 搜索服务，执行仍经现有审批代理；适配器不授予新权限。 */
export class WebSearchTool {
  constructor(tools) { this.tools = tools; this.stages = new WeakMap(); }
  available(context, descriptor) {
    const stage = this.stages.get(context);
    const query = descriptor.name === 'web.search' || isPublicSearchTool(descriptor), page = isPublicFetchTool(descriptor);
    if (!stage || !query && !page) return true;
    if (performance.now() - stage.started >= stage.durationMs) return false;
    return query ? stage.queryCount < stage.queries : stage.pageCount < stage.pages;
  }
  async providers() {
    const config = await this.tools.getConfig(), states = new Map(this.tools.mcp.diagnostics().map(item => [item.serverId, item]));
    const settings = this.tools.retrieval ? await this.tools.retrieval.effective(null) : null;
    const discovered = new Map();
    for (const pending of this.tools.mcp.connections.values()) {
      let timer;
      // Inspect resolved connections only; settings must not wait for or start a handshake.
      // 仅检查已完成连接；设置读取不等待或启动新的握手。
      const connection = await Promise.race([Promise.resolve(pending).catch(() => null),
        new Promise(resolve => { timer = setTimeout(() => resolve(null), 0); timer.unref(); })]).finally(() => clearTimeout(timer));
      if (!connection?.closed) for (const tool of connection?.tools ?? []) {
        const kind = providerKind(tool); if (kind) discovered.set(connection.serverId, kind);
      }
    }
    const providers = config.mcpServers.filter(server => /(?:^|[-_.])(?:exa|brave(?:-search)?)(?:[-_.]|$)/iu.test(server.id) ||
      (server.args ?? []).some(argument => /(?:exa-mcp-server|brave-search-mcp|server-brave-search)/u.test(argument)) || discovered.has(server.id));
    return { selectedId: settings?.web.providerId ?? 'auto', providers: providers.map(server => {
      const state = states.get(server.id);
      return { id: server.id, name: discovered.get(server.id) === 'brave' || /brave/iu.test(server.id + JSON.stringify(server.args)) ? 'Brave Search' : 'Exa',
        enabled: server.enabled, ready: server.enabled && ['ready', 'connected'].includes(state?.state),
        state: state?.state ?? 'disconnected', origin: server.origin ?? 'user' };
    }) };
  }
  async settings(context) {
    return this.tools.retrieval ? this.tools.retrieval.effective(context.projectId) : { web: { mode: 'auto', depth: 'standard', providerId: 'auto' } };
  }
  async take(context, kind) {
    const settings = await this.settings(context);
    if (settings.web.mode === 'off') throw toolFailure('网页检索已关闭。', 'WEB_SEARCH_DISABLED', 409);
    let stage = this.stages.get(context);
    if (!stage) {
      stage = { ...limits[settings.web.depth], started: performance.now(), queryCount: 0, pageCount: 0, seen: new Set() };
      this.stages.set(context, stage);
    }
    if (performance.now() - stage.started >= stage.durationMs || (kind === 'query' ? stage.queryCount >= stage.queries : stage.pageCount >= stage.pages))
      throw toolFailure('本次网页检索预算已用完，请根据已有证据回答或说明不足；其他代码任务可继续。', 'WEB_STAGE_BUDGET_EXHAUSTED', 409);
    if (kind === 'query') stage.queryCount++; else stage.pageCount++;
    return { settings, remainingMs: Math.max(1, stage.durationMs - (performance.now() - stage.started)), stage };
  }
  async run(context, input, options) {
    const { settings } = await this.settingsAndCheck(context);
    if (!this.tools.retrieval) await this.take(context, 'query');
    const snapshot = this.tools.catalogs.get(context);
    const candidates = [...(snapshot?.descriptors.values() ?? [])].filter(isPublicSearchTool);
    const descriptor = candidates.find(item => settings.web.providerId === 'auto' || item.serverId === settings.web.providerId);
    if (!descriptor) throw toolFailure('没有就绪的搜索服务，请启用 Exa 或 Brave MCP。', 'WEB_SEARCH_PROVIDER_UNAVAILABLE', 503);
    const providerArguments = { query: input.query };
    const properties = descriptor.originalInputSchema?.properties ?? descriptor.inputSchema?.properties?.arguments?.properties ?? {};
    const count = Math.min(input.limit ?? 6, limits[settings.web.depth].pages);
    if (properties.numResults) providerArguments.numResults = count;
    if (properties.count) providerArguments.count = count;
    if (properties.search_lang && settings.web.language !== 'auto')
      providerArguments.search_lang = settings.web.language === 'zh-CN' ? 'zh-hans' : 'en';
    const result = await this.tools.execute(context, { id: randomUUID(), name: descriptor.name,
      arguments: { arguments: providerArguments, policy: { reason: input.reason } } }, options);
    if (result.isError) return { value: { message: result.content, originalResultRef: result.resultRef }, isError: true, code: result.code };
    const sources = normalizeWebSources(result.content, count);
    let newEvidence = 0;
    for (const source of sources) {
      const identity = createHash('sha256').update(source.url + source.excerpt).digest('hex');
      const activeStage = this.stages.get(context);
      if (!activeStage.seen.has(identity)) { newEvidence++; activeStage.seen.add(identity); }
    }
    return { value: { sources, providerId: descriptor.serverId, originalResultRef: result.resultRef,
      newEvidence, stopSuggested: sources.length > 0 && newEvidence === 0,
      evidenceDecision: { sufficiency: 'not-evaluated', missingInformation: input.gap ?? null,
        next: sources.length ? 'answer-if-supported-otherwise-read-the-specific-missing-page' : 'state-the-gap-or-adjust-the-query',
        notice: 'Check entity, time, scope and omitted conditions. Answer when those requested facts have sources. New URLs or matching keywords alone do not prove sufficiency. Another search should address a concrete gap; failed search is not permission to open a local browser.' },
      remainingQueries: Math.max(0, this.stages.get(context).queries - this.stages.get(context).queryCount) }, outsideWorkspace: true };
  }

  async settingsAndCheck(context) {
    const settings = await this.settings(context);
    if (settings.web.mode === 'off') throw toolFailure('网页检索已关闭。', 'WEB_SEARCH_DISABLED', 409);
    return { settings };
  }
}
