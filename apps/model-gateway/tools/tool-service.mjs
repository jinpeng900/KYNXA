import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { realpath } from 'node:fs/promises';
import { isDeepStrictEqual } from 'node:util';
import { validateId } from '../platform/conversation-id.mjs';
import { runResourceTask } from '../platform/resources/resource-task.mjs';
import { buildToolSystemPrompt } from './tool-system-prompt.mjs';
import { ToolStorageBoundary } from './tool-storage-boundary.mjs';
import { changedMcpServerIds, hasToolConfigurationChanged, isConfiguredToolEnabled } from './tool-configuration.mjs';
import { AgentConfigRepository } from './agent-config.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { OFFICIAL_TOOLS_ROOT, curatedMcpPresets, readOfficialToolsManifest, normalizeOfficialDisabledSkills } from './official-tools.mjs';
import { AppSkillService } from './skill-service.mjs';
import { McpToolClients, isMcpExecutionNotDispatched } from './mcp-client.mjs';
import { executeFilesystem } from './filesystem-tools.mjs';
import { WebFetchTool } from './web-fetch.mjs';
import { WebSearchTool, isPublicSearchTool, isPublicFetchTool, isAutomaticBrowserRead } from './retrieval/web-search.mjs';
import { validatePublicWebUrl } from './web-http-transport.mjs';
import { needsToolApproval, ToolApprovalRegistry } from './tool-policy.mjs';
import { bindLocalPath, revalidateLocalPathBinding, boundedInteger, inspectLocalPath, objectInput, resolveToolPath, toolFailure, within } from '../platform/tool-paths.mjs';
import { ModelToolCatalog } from './tool-catalog.mjs';
import { searchTools, toolSelectionSignals } from './tool-discovery.mjs';
import { resolveToolExecutionEnvironment } from './tool-execution-environment.mjs';
import { browserConnectionPrompt, isBackgroundBrowserConnection } from './browser-connections.mjs';
import { assertBrowserLaunchAllowed, assertBrowserServerAllowed, canUseBrowserServer, inferBrowserTaskIntent, isBrowserTaskFollowUp, isBrowserApplicationPath } from './browser-intent-policy.mjs';
import { ToolResultStore, previewToolResult, publicToolResult } from '../data/tool-result-store.mjs';
import { supportsSkillExecution, verifiesSkillExecution } from './sandbox-skill.mjs';
import { extensionPointerPath } from '../data/extension-storage.mjs';
import { executeHistoryTool } from './tool-history.mjs';
import { canRunInParallel } from './tool-scheduling.mjs';
import { RequestObservationCache, canReuseObservation, observationFingerprint } from './tool-observations.mjs';
import { ConversationWorkspaces } from '../data/sandbox-workspaces.mjs';
import { isDesktopObservation } from './tool-outcomes.mjs';
import { inferBrowserInteractionPolicy, isExplicitForegroundForbidden } from './browser-sessions.mjs';
import { prepareDesktopLaunchArguments } from './desktop-launch-options.mjs';
import { isSensitiveFilePath } from './sensitive-files.mjs';
import { projectEvidenceSearchResult, projectRetrievalModelView } from '../data/retrieval/evidence-references.mjs';
import { HostTerminalJobs } from './host-terminal-jobs.mjs';

const MAX_TOOL_INPUT_BYTES = 2 * 1024 * 1024;
const MAX_TOOL_RESULT_CHARS = 65536;
const filesystemReadTools = new Set(['filesystem.read', 'filesystem.list', 'filesystem.search', 'filesystem.stat']);
const safeErrorCode = (error, fallback = 'TOOL_FAILED') => typeof error?.code === 'string' && /^[A-Z0-9_]{1,100}$/.test(error.code)
  ? error.code : fallback;
const GENERIC_MCP_STARTUP_TERMS = new Set(['the', 'this', 'that', 'these', 'those', 'and', 'for', 'with', 'from', 'into', 'about',
  'please', 'check', 'show', 'read', 'write', 'find', 'get', 'use', 'open', 'run', 'tool', 'tools', 'service', 'services',
  'server', 'servers', 'local', 'current', 'now', 'help', '工具', '服务', '使用', '检查', '查看', '打开', '读取', '运行', '当前', '本地', '这些', '这个', '那个']);

function publicDescriptor(descriptor) {
  return { name: descriptor.name, description: descriptor.description, inputSchema: structuredClone(descriptor.inputSchema), source: descriptor.source,
    ...(descriptor.toolName ? { rawName: descriptor.toolName } : {}), enabled: descriptor.enabled !== false,
    ...(descriptor.executionEnvironment ? { executionEnvironment: descriptor.executionEnvironment } : {}),
    ...(descriptor.available !== undefined ? { available: descriptor.available,
      ...(descriptor.unavailableCode ? { unavailableCode: descriptor.unavailableCode } : {}) } : {}) };
}

function relevantMcpServerIds(servers, context) {
  const signals = context?.toolIntent ?? toolSelectionSignals(context?.message ?? '');
  const retainedNames = signals.retainedNames ?? new Set();
  const explicitBrowser = context?.browserTaskIntent?.explicitBrowserTask === true;
  return servers.filter(server => {
    const label = `${server.id} ${server.name}`.normalize('NFKC').toLowerCase();
    const components = new Set(label.match(/[a-z0-9]+/gu) ?? []);
    if ([...retainedNames].some(name => name.startsWith(`mcp.${server.id}.`))) return true;
    if (/playwright|chrome[-_.]?devtools|puppeteer|browser/.test(label)) return explicitBrowser;
    if (signals.web && /exa|brave|search/.test(label)) return true;
    if (signals.docs && /context7|deepwiki|documentation|docs/.test(label)) return true;
    // Warm-up clues rank startup candidates only. Latin words match complete
    // identifier components; Chinese names retain character matching. Discovery
    // and permission are unchanged when a generic word does not warm a service.
    // 预热线索仅选择启动候选；拉丁词匹配完整标识符组件，中文名称保留字符匹配，忽略通用词不影响发现或权限。
    return (signals.terms ?? []).some(term => !GENERIC_MCP_STARTUP_TERMS.has(term) &&
      (/\p{Script=Han}/u.test(term) ? term.length >= 2 && label.includes(term) : term.length >= 3 &&
        (components.has(term) || term === server.id.toLowerCase())));
  }).slice(0, 4).map(server => server.id);
}

function nativeToolAvailability(context, descriptor) {
  if (!context) return {};
  if (descriptor.name.startsWith('computer.')) {
    const available = context.desktopCapabilities.available === true && context.desktopCapabilities.boundary === 'host-desktop' &&
      context.desktopCapabilities.operations?.includes(descriptor.name.slice('computer.'.length));
    return { available: Boolean(available), ...(available ? {} : { unavailableCode: 'DESKTOP_UNAVAILABLE' }) };
  }
  if (descriptor.name.startsWith('terminal.host.')) {
    const available = context.hostTerminalCapabilities.available === true && context.hostTerminalCapabilities.boundary === 'host-terminal' &&
      (descriptor.name === 'terminal.host.run' || context.hostTerminalCapabilities.backgroundJobs === true);
    return { available, ...(available ? {} : { unavailableCode: 'HOST_TERMINAL_UNAVAILABLE' }) };
  }
  if (descriptor.name === 'terminal.run' || descriptor.name === 'skill.run') {
    if (context.workspaceDiagnostic) return { available: false, unavailableCode: 'WORKSPACE_UNAVAILABLE' };
    const sandbox = context.sandboxCapabilities;
    const available = sandbox.available === true && sandbox.sandbox === 'appcontainer' && sandbox.failClosed === true &&
      sandbox.checksChildToken === true && (descriptor.name !== 'skill.run' || supportsSkillExecution(sandbox));
    return { available, ...(available ? {} : { unavailableCode: 'SANDBOX_UNAVAILABLE' }) };
  }
  return {};
}

function boundedContent(content) {
  const marker = '\n[Tool result truncated]';
  if (content.length <= MAX_TOOL_RESULT_CHARS) return content;
  let preview = content.slice(0, MAX_TOOL_RESULT_CHARS - marker.length);
  if (/[\uD800-\uDBFF]$/.test(preview)) preview = preview.slice(0, -1);
  return preview + marker;
}

function validateBuiltinInput(descriptor, input) {
  const schema = descriptor.inputSchema;
  for (const required of schema.required ?? []) if (!Object.hasOwn(input, required)) throw toolFailure(`缺少工具参数 ${required}。`);
  for (const [key, value] of Object.entries(input)) {
    const property = schema.properties[key];
    if (!property) throw toolFailure(`不支持工具参数 ${key}。`);
    const types = Array.isArray(property.type) ? property.type : [property.type];
    const type = value === null ? 'null' : Array.isArray(value) ? 'array' : typeof value;
    if (!(types.includes(type) || (types.includes('integer') && Number.isSafeInteger(value)))) throw toolFailure(`工具参数 ${key} 类型无效。`);
    if (property.enum && !property.enum.includes(value)) throw toolFailure(`工具参数 ${key} 值无效。`);
    if (typeof value === 'string' && ((property.minLength !== undefined && value.length < property.minLength) ||
        (property.maxLength !== undefined && value.length > property.maxLength))) throw toolFailure(`工具参数 ${key} 长度无效。`);
    if (typeof value === 'number' && ((property.minimum !== undefined && value < property.minimum) || (property.maximum !== undefined && value > property.maximum)))
      throw toolFailure(`工具参数 ${key} 超出范围。`);
    if (Array.isArray(value) && (value.length > (property.maxItems ?? 64) || value.some(item => typeof item !== property.items?.type || item.includes('\0'))))
      throw toolFailure(`工具参数 ${key} 无效。`);
  }
}

/**
 * Tool authority is derived from canonical work ownership and immutable turn context, never model metadata.
 * 工具权限来自正式工作归属和不可变轮次上下文，不来自模型元信息。
 */
export class ToolService {
  constructor({ conversationStore, dataHome, extensionRoot, extensionPointer, sandboxRunner, desktopRunner, hostTerminalRunner, webFetcher, approvalTimeoutMs, bundledDirectory, officialTools = bundledDirectory !== null } = {}) {
    if (!conversationStore?.root || !dataHome) throw toolFailure('缺少工具存储上下文。');
    this.conversations = conversationStore;
    this.root = resolve(conversationStore.root);
    this.webFetcher = webFetcher ?? new WebFetchTool();
    this.dataHome = resolve(dataHome);
    this.extensionRoot = resolve(extensionRoot ?? this.root);
    this.extensionPointer = extensionPointer ?? extensionPointerPath();
    this.storageBoundary = new ToolStorageBoundary({ root: this.root, dataHome: this.dataHome,
      extensionRoot: this.extensionRoot, extensionPointer: this.extensionPointer, officialToolsRoot: OFFICIAL_TOOLS_ROOT });
    this.officialTools = officialTools;
    this.config = new AgentConfigRepository(this.extensionRoot, officialTools ? { officialPresets: curatedMcpPresets,
      officialToolsRoot: OFFICIAL_TOOLS_ROOT, normalizeDisabledSkills: normalizeOfficialDisabledSkills } : {});
    this.skills = new AppSkillService(this.extensionRoot, { bundledDirectory, ownedDataRoot: this.root,
      denyResource: path => this.storageBoundary.isCredential(path) || this.storageBoundary.isPrivateResult(path) });
    if (this.skills.bundledDirectory && !this.storageBoundary.roots.includes(this.skills.bundledDirectory))
      this.storageBoundary.roots.push(this.skills.bundledDirectory);
    this.results = new ToolResultStore({ conversationStore });
    this.mcp = new McpToolClients({ extensionRoot: this.extensionRoot });
    this.sandboxRunner = sandboxRunner;
    this.desktopRunner = desktopRunner;
    this.hostTerminalRunner = hostTerminalRunner;
    this.hostTerminalJobs = hostTerminalRunner?.run ? new HostTerminalJobs({ runner: hostTerminalRunner }) : null;
    this.workspaces = new ConversationWorkspaces({ root: this.dataHome });
    this.approvals = new ToolApprovalRegistry({ ...(approvalTimeoutMs ? { timeoutMs: approvalTimeoutMs } : {}) });
    this.contexts = new WeakSet();
    this.taskVerifications = new WeakMap();
    this.evidenceBudgets = new WeakMap();
    this.catalogs = new WeakMap();
    this.stages = new WeakMap();
    this.observationCaches = new WeakMap();
    this.discoveryRefreshes = new WeakSet();
    this.configGeneration = 0;
    this.closed = false;
  }

  async getConfig() {
    await this.storageBoundary.refresh();
    const config = await this.config.read();
    if (this.officialTools) config.officialPackageVersion = (await readOfficialToolsManifest()).version;
    return config;
  }

  async updateConfig(input) {
    const current = await this.getConfig();
    const value = await this.config.update(input);
    if (this.officialTools) value.officialPackageVersion = (await readOfficialToolsManifest()).version;
    this.mcp.updateServerPolicies(value.mcpServers);
    // Revision-only saves must not destroy browser state or revoke otherwise unchanged calls.
    // A concurrent update may have advanced the repository after our read; then fail safe.
    // 只有修订号变化的保存不能破坏浏览器状态或撤销未变化调用；若并发更新已推进仓储，则保守拒绝旧状态。
    if (current.revision === input.expectedRevision &&
        isDeepStrictEqual({ ...current, revision: 0 }, { ...value, revision: 0 })) return value;
    this.configGeneration++;
    this.liveConfig = structuredClone(value);
    // Tool toggles revoke their own calls; unrelated skills, files and browser connections remain usable.
    // 工具启停仅撤销自身调用，无关技能、文件工具及浏览器连接继续可用。
    const changedServers = changedMcpServerIds(current, value);
    if (changedServers.length) await this.mcp.resetServers(changedServers);
    return value;
  }

  async _ownership(conversationId) {
    const catalog = await this.conversations.catalog();
    const same = id => validateId(id).toLowerCase() === conversationId;
    for (const project of catalog.Projects) if (project.Chats.some(chat => same(chat.Id))) {
      const projectId = validateId(project.Id).toLowerCase();
      const workspaceRoot = project.IsFolderlessWorkspace || !project.FolderPath || !isAbsolute(project.FolderPath) ? null : resolve(project.FolderPath);
      const managedParent = join(this.root, 'Desktop', 'Projects');
      let managedWorkspace = false;
      if (workspaceRoot && within(managedParent, dirname(workspaceRoot)) && within(dirname(workspaceRoot), managedParent)) {
        try { managedWorkspace = validateId(basename(workspaceRoot)).toLowerCase() === projectId; } catch { /* Not a canonical project folder. 此路径不是正式项目目录。 */ }
      }
      return { projectId, workspaceRoot, managedWorkspace };
    }
    if (catalog.Chats.some(chat => same(chat.Id))) return { projectId: null, workspaceRoot: null, managedWorkspace: false };
    // Preserve the formal store's distinct deleted/not-found error instead of fabricating a conversation.
    // 保留正式存储明确的已删除或不存在错误，不虚构会话。
    await this.conversations.describeConversation(conversationId);
    throw toolFailure('聊天不存在。', 'CONVERSATION_NOT_FOUND', 404);
  }

  async _sandboxCapabilities() {
    if (!this.sandboxRunner?.capabilities) return { available: false, reason: 'SANDBOX_UNAVAILABLE' };
    try { return await this.sandboxRunner.capabilities(); }
    catch { return { available: false, reason: 'SANDBOX_UNAVAILABLE' }; }
  }

  async _usesConversationWorkspace(ownership) {
    if (ownership.workspaceRoot === null) return true;
    if (!ownership.managedWorkspace) return false;
    try { await inspectLocalPath(ownership.workspaceRoot, { allowMissing: true }); }
    catch (error) {
      // Only our exact legacy project directory may use a separate chat workspace.
      // An explicitly mounted folder never gains a link-traversal exception.
      // 只有本应用精确识别的旧项目目录可改用独立聊天工作区，明确挂载的目录不获得链接遍历例外。
      if (error.code === 'UNSAFE_TOOL_PATH') return true;
      throw error;
    }
    return false;
  }

  async _assertOwnership(context) {
    const current = await this._ownership(context.conversationId);
    if (current.projectId !== context.projectId || current.workspaceRoot !== context.linkedWorkspaceRoot ||
        await this._usesConversationWorkspace(current) !== context.isolatedWorkspace)
      throw toolFailure('聊天工作范围已变化，此工具调用已停止。', 'WORKSPACE_CHANGED', 409);
    if (context.isolatedWorkspace) await this.workspaces.verify(context.conversationId, context.workspaceRoot);
    if (context.workspaceBinding) await revalidateLocalPathBinding(context.workspaceBinding);
  }

  async _desktopCapabilities() {
    if (!this.desktopRunner?.capabilities) return { available: false, boundary: 'host-desktop', operations: [] };
    try { return await this.desktopRunner.capabilities(); }
    catch { return { available: false, boundary: 'host-desktop', operations: [] }; }
  }

  async _hostTerminalCapabilities() {
    if (!this.hostTerminalRunner?.capabilities) return { available: false, boundary: 'host-terminal', shells: [] };
    try { return await this.hostTerminalRunner.capabilities(); }
    catch { return { available: false, boundary: 'host-terminal', shells: [] }; }
  }

  async createContext(conversationId, { requestId, permissionMode = 'ask', message = '', previousUserMessages: trustedUserMessages } = {}) {
    if (this.closed) throw toolFailure('工具服务已关闭。', 'TOOL_SERVICE_CLOSED', 409);
    conversationId = validateId(conversationId).toLowerCase();
    requestId = validateId(requestId).toLowerCase();
    if (!['ask', 'smart', 'full'].includes(permissionMode)) throw toolFailure('工具权限模式无效。');
    const ownership = await this._ownership(conversationId);
    await this.storageBoundary.refresh();
    const isolatedWorkspace = await this._usesConversationWorkspace(ownership);
    let workspaceRoot = isolatedWorkspace ? await this.workspaces.ensure(conversationId) : ownership.workspaceRoot;
    let workspaceBinding, workspaceDiagnostic;
    if (!isolatedWorkspace) {
      try {
        workspaceBinding = await bindLocalPath(workspaceRoot);
        workspaceRoot = workspaceBinding.path;
      } catch (error) {
        // A missing/moved work folder must not disable ordinary chat or independent host operations.
        // 工作文件夹丢失或迁移不能禁用普通聊天与独立宿主操作。
        if (!['ENOENT', 'ENOTDIR', 'EACCES', 'EPERM', 'UNSAFE_TOOL_PATH'].includes(error.code)) throw error;
        workspaceDiagnostic = safeErrorCode(error, 'WORKSPACE_UNAVAILABLE');
      }
    }
    const [sandbox, desktop, hostTerminal] = await Promise.all([this._sandboxCapabilities(), this._desktopCapabilities(), this._hostTerminalCapabilities()]);
    const sandboxCapabilities = Object.freeze(sandbox), desktopCapabilities = Object.freeze(desktop);
    // Related instructions continue an authorized browser task; unrelated research starts with background retrieval.
    // 相关后续指令承接已授权浏览器任务，无关资料检索仍从后台查询开始。
    const previousUserMessages = isBrowserTaskFollowUp(message)
      ? trustedUserMessages ?? (await this.conversations.readModelMessages(conversationId)).filter(item => item.Role === 'user').map(item => item.Content ?? '') : [];
    const context = Object.freeze({ conversationId, requestId, permissionMode, message, ...ownership,
      linkedWorkspaceRoot: ownership.workspaceRoot, workspaceRoot, isolatedWorkspace,
      ...(workspaceBinding ? { workspaceBinding } : {}), ...(workspaceDiagnostic ? { workspaceDiagnostic } : {}),
      managedWorkspace: ownership.managedWorkspace || ownership.workspaceRoot === null, sandboxCapabilities, desktopCapabilities,
      hostTerminalCapabilities: Object.freeze(hostTerminal),
      toolIntent: Object.freeze(toolSelectionSignals(message, { historySignals: previousUserMessages })),
      browserInteraction: Object.freeze(inferBrowserInteractionPolicy(message)),
      browserTaskIntent: Object.freeze(inferBrowserTaskIntent(message, previousUserMessages)),
      foregroundForbidden: isExplicitForegroundForbidden(message),
      extensionRoot: this.extensionRoot });
    this.contexts.add(context);
    this.stages.set(context, new Set());
    return context;
  }

  _assertContext(context) {
    if (this.closed || !context || !this.contexts.has(context)) throw toolFailure('工具上下文无效或已关闭。', 'INVALID_TOOL_CONTEXT', 409);
  }

  registerTaskVerification(context, readVerification) {
    this._assertContext(context);
    if (typeof readVerification === 'function') this.taskVerifications.set(context, readVerification);
  }

  taskVerificationFor(context) { this._assertContext(context); return this.taskVerifications.get(context)?.(); }

  setEvidenceBudget(context, maximumTokens) {
    this._assertContext(context);
    this.evidenceBudgets.set(context, Math.max(0, Math.min(32768, Math.floor(maximumTokens))));
  }

  async listSkills(context, options) {
    if (context) this._assertContext(context);
    return this.skills.list(context, await this.getConfig(), options);
  }

  async readSkill(id, context) {
    if (context) this._assertContext(context);
    return this.skills.read(id, context, await this.getConfig());
  }

  async checkSkill(id, context) {
    if (context) this._assertContext(context);
    return this.skills.checkEnvironment(id, context, await this.getConfig(),
      { sandboxCapabilities: context?.sandboxCapabilities ?? await this._sandboxCapabilities() });
  }

  async catalog(context, { connectMcp = false, refreshMcpCatalog = false, includeDisabled = false, mcpServerIds } = {}) {
    if (context) this._assertContext(context);
    const previousSnapshot = context ? this.catalogs.get(context) : undefined;
    const generation = this.configGeneration;
    const config = await this.getConfig();
    const allowedServers = config.mcpServers.filter(server => canUseBrowserServer(context, server));
    const selectedServerIds = mcpServerIds ?? (connectMcp === 'relevant' ? relevantMcpServerIds(allowedServers, context) : undefined);
    const remote = await this.mcp.catalog({ ...config, mcpServers: allowedServers }, context,
      { connect: Boolean(connectMcp), refresh: refreshMcpCatalog, serverIds: selectedServerIds });
    const effectiveConfig = generation === this.configGeneration ? config : await this.getConfig();
    const all = [...builtinDescriptors.map(tool => ({ ...tool, ...nativeToolAvailability(context, tool) })), ...remote.map(tool => ({ ...tool,
      enabled: isConfiguredToolEnabled(effectiveConfig, tool) && !hasToolConfigurationChanged(config, effectiveConfig, tool) }))]
      .map(tool => ({ ...tool, executionEnvironment: resolveToolExecutionEnvironment(tool,
        { servers: effectiveConfig.mcpServers, context }) }));
    const retrievalSettings = context && this.retrieval ? await this.retrieval.effective(context.projectId) : null;
    const servers = new Map(effectiveConfig.mcpServers.map(server => [server.id, server]));
    const descriptors = all.filter(tool => tool.enabled !== false && tool.available !== false && canUseBrowserServer(context, servers.get(tool.serverId)) &&
      (!tool.name.startsWith('knowledge.') || this.retrieval && retrievalSettings?.local.enabled !== false) &&
      (retrievalSettings?.web.browserRead !== 'off' || !isAutomaticBrowserRead(context, tool)) &&
      (retrievalSettings?.web.mode !== 'off' || !(tool.name.startsWith('web.') || isPublicSearchTool(tool) || isPublicFetchTool(tool))));
    const mcpDiscovery = this.mcp.discovery({ ...effectiveConfig,
      mcpServers: effectiveConfig.mcpServers.filter(server => canUseBrowserServer(context, server)) }, context);
    // Remember revocation for accurate diagnostics after rediscovery, never as
    // an executable declaration or preserved authorization.
    // 再发现后保留撤销分类以准确诊断，不能将其当成可执行声明或保留的授权。
    const revokedNames = new Set(previousSnapshot?.revokedNames ?? []);
    for (const descriptor of previousSnapshot?.descriptors.values() ?? [])
      if (descriptor.source.startsWith('mcp:') && !isConfiguredToolEnabled(effectiveConfig, descriptor)) revokedNames.add(descriptor.name);
    for (const descriptor of descriptors) revokedNames.delete(descriptor.name);
    if (context) this.catalogs.set(context, { generation, config: structuredClone(config), descriptors: new Map(descriptors.map(item => [item.name, item])),
      inventory: new Map(all.map(item => [item.name, item])), servers, mcpDiscovery, revokedNames,
      browserPrompt: browserConnectionPrompt(effectiveConfig.mcpServers.filter(server => descriptors.some(tool => tool.serverId === server.id))) });
    return (includeDisabled ? all : descriptors).map(publicDescriptor);
  }

  configureModelCatalog(context, options) {
    this._assertContext(context);
    const snapshot = this.catalogs.get(context);
    snapshot.modelOptions = structuredClone(options);
    snapshot.model = new ModelToolCatalog([...snapshot.descriptors.values()].map(publicDescriptor), options);
    return snapshot.model.wire();
  }

  modelCatalog(context) {
    this._assertContext(context);
    const snapshot = this.catalogs.get(context);
    return (snapshot?.model?.wire() ?? []).filter(tool =>
      isConfiguredToolEnabled(this.liveConfig ?? snapshot.config, snapshot.descriptors.get(tool.name) ?? tool) &&
      (!this.webSearch || this.webSearch.available(context, tool)));
  }

  async _assertToolConfiguration(context, descriptor, args) {
    const snapshot = this.catalogs.get(context);
    if (!snapshot || snapshot.generation === this.configGeneration) return;
    if (hasToolConfigurationChanged(snapshot.config, await this.getConfig(), descriptor, args))
      throw Object.assign(toolFailure('此工具的配置或权限已变化，请重新发现该能力；无关工具可以继续。', 'AGENT_CONFIG_CHANGED', 409),
        { toolConfigurationRevoked: true });
  }

  async _refreshDiscoveryOnce(context, snapshot) {
    if (!snapshot || this.discoveryRefreshes.has(context) ||
        snapshot.generation === this.configGeneration && !this.mcp.errors.size) return snapshot;
    this.discoveryRefreshes.add(context);
    const selected = snapshot.model?.selected.map(tool => tool.name) ?? [];
    await this.catalog(context);
    const refreshed = this.catalogs.get(context);
    if (snapshot.modelOptions) {
      this.configureModelCatalog(context, snapshot.modelOptions);
      const retained = selected.filter(name => refreshed.descriptors.has(name));
      if (retained.length) {
        try { refreshed.model.load(retained); }
        catch (error) {
          // A larger refreshed schema must not block discovery; retain the fresh bounded selection instead.
          // 刷新后的 schema 变大不能阻断目录发现，保留新目录按预算选定的工具即可。
          if (error.code !== 'TOOL_CATALOG_BUDGET') throw error;
        }
      }
    }
    return refreshed;
  }

  /** Connecting requested metadata is separate from executing a tool. Preserve
   * request model selection while replacing cached schemas with the live catalog.
   * 连接所请求的元信息与执行工具分开；用实时目录替换缓存 schema 时保留本请求的模型工具选择。
   */
  async _connectRequestedServers(context, serverIds) {
    const snapshot = this.catalogs.get(context), selected = snapshot?.model?.selected.map(tool => tool.name) ?? [];
    await this.catalog(context, { connectMcp: true, mcpServerIds: [...new Set(serverIds)] });
    const refreshed = this.catalogs.get(context);
    if (snapshot?.modelOptions) {
      this.configureModelCatalog(context, snapshot.modelOptions);
      const retained = selected.filter(name => refreshed.descriptors.has(name));
      if (retained.length) {
        try { refreshed.model.load(retained); }
        catch (error) { if (error.code !== 'TOOL_CATALOG_BUDGET') throw error; }
      }
    }
    return refreshed;
  }

  async _assertDesktopForegroundPolicy(context, call, signal) {
    if (!context.foregroundForbidden) return;
    const action = call.name.slice('computer.'.length);
    const mayActivate = ['activate', 'move', 'click', 'scroll', 'drag', 'type', 'key'].includes(action) ||
      action === 'window' && ['maximize', 'restore'].includes(call.arguments.mode);
    if (!mayActivate) return;
    let target = { kind: 'application' };
    if (!isExplicitForegroundForbidden(context.message, target)) {
      // Bind a named prohibition to the observed window, never to an unrelated app or model-supplied title.
      // 点名禁止须绑定实际观察的窗口，不能扩散到其他软件或相信模型传来的标题。
      const listed = await this.desktopRunner.run('windows', { processId: call.arguments.processId,
        reason: 'Verify the target application before applying the user foreground constraint.' }, signal);
      const window = listed.value?.windows?.find(item => item.windowId === call.arguments.windowId && item.processId === call.arguments.processId);
      if (!window) throw toolFailure('目标窗口身份无法确认，请重新列出窗口。', 'DESKTOP_TARGET_CHANGED', 409);
      target = { kind: isBrowserApplicationPath(window.executablePath ?? '') ? 'browser' : 'application',
        appPath: window.executablePath, applicationName: window.processName, title: window.title };
    }
    if (isExplicitForegroundForbidden(context.message, target))
      throw toolFailure('用户明确禁止激活此目标窗口，请使用后台操作方式。', 'DESKTOP_FOREGROUND_FORBIDDEN', 403);
  }

  async refreshMcp(context, options = {}) {
    if (context) this._assertContext(context);
    return this.catalog(context, { ...options, connectMcp: true, refreshMcpCatalog: true });
  }

  async systemPrompt(context, { maximumTokens = Infinity } = {}) {
    this._assertContext(context);
    const skills = await this.listSkills(context);
    return buildToolSystemPrompt(context, { skills,
      deviceCapabilities: context.toolIntent?.deviceState ? this.capabilitySnapshot(context) : undefined,
      browserPrompt: this.catalogs.get(context)?.browserPrompt,
      unavailableSkillCount: this.skills.discovery.get(skills)?.unavailableCount,
      mcpErrorIds: [...this.mcp.errors].map(([id, code]) => `${id} (${safeErrorCode({ code }, 'MCP_UNAVAILABLE')})`), maximumTokens });
  }

  approve(input) { return this.approvals.approve(input); }

  capabilitySnapshot(context) {
    this._assertContext(context);
    const snapshot = this.catalogs.get(context);
    const names = ['terminal.host.run', 'terminal.run', 'web.fetch', 'computer.windows'];
    return { checkedBy: 'kynxa-runtime', discoveryRequiresApproval: false, discoveryExecutesCommands: false,
      scope: 'gateway-host', userDeviceRelationship: 'unverified',
      tools: names.map(name => {
        const descriptor = snapshot?.inventory?.get(name) ?? builtinDescriptors.find(item => item.name === name);
        const availability = nativeToolAvailability(context, descriptor);
        const available = snapshot?.descriptors.has(name) === true;
        return { name, state: available ? 'available' : availability.available === false ? 'unavailable' : snapshot ? 'disabled' : 'not-discovered',
          schema: !snapshot?.model ? 'selection-pending' : snapshot.model.selected.some(tool => tool.name === name) ? 'loaded' : 'deferred',
          ...(availability.unavailableCode ? { code: availability.unavailableCode } : {}),
          approval: context.permissionMode === 'full' || ['web.fetch', 'computer.windows'].includes(name)
            ? 'not-required-by-current-mode' : 'execution-policy-applies' };
      }) };
  }

  executionEnvironmentFor(context, name) {
    const snapshot = this.catalogs.get(context);
    const descriptor = snapshot?.inventory?.get(name) ?? builtinDescriptors.find(item => item.name === name);
    return resolveToolExecutionEnvironment(descriptor ?? {}, { servers: [...(snapshot?.servers?.values() ?? [])], context });
  }

  async execute(context, call, { signal, emit, interactive = true, onApprovalWait = () => {} } = {}) {
    let outsideWorkspace = false;
    let executionStarted = false;
    let mcpDispatchAttempted = false;
    let preparedSkill;
    let webStage;
    let webDeadline;
    let pathBinding;
    const originalSignal = signal;
    try {
      this._assertContext(context);
      objectInput(call); objectInput(call.arguments);
      if (typeof call.id !== 'string' || !call.id || call.id.length > 128 || /[\0\r\n]/.test(call.id) || typeof call.name !== 'string') throw toolFailure('工具调用身份无效。');
      if (Buffer.byteLength(JSON.stringify(call.arguments)) > MAX_TOOL_INPUT_BYTES) throw toolFailure('工具参数过大。');
      call = { id: call.id, name: call.name, arguments: structuredClone(call.arguments) };
      // Freeze business arguments before the first asynchronous boundary, including approval-free reads.
      // 在首个异步边界前固定业务参数，包括无需审批的只读调用。
      await this.storageBoundary.refresh();
      let snapshot = this.catalogs.get(context);
      const descriptor = snapshot?.descriptors.get(call.name) ?? builtinDescriptors.find(item => item.name === call.name);
      if (!descriptor && snapshot?.revokedNames?.has(call.name))
        throw Object.assign(toolFailure('此工具曾可用，但当前配置已撤销；请发现其他能力。', 'AGENT_CONFIG_CHANGED', 409),
          { toolConfigurationRevoked: true });
      if (!descriptor) throw toolFailure('工具不存在或尚未发现。', 'TOOL_NOT_FOUND', 404);
      await this._assertToolConfiguration(context, descriptor, call.arguments);
      if (descriptor.source.startsWith('mcp:'))
        assertBrowserServerAllowed(context, snapshot?.servers?.get(descriptor.serverId) ?? this.mcp.servers.get(descriptor.serverId));
      if (this.retrieval && isAutomaticBrowserRead(context, descriptor) &&
          (await this.retrieval.effective(context.projectId)).web.browserRead === 'off')
        throw toolFailure('自动浏览器阅读已关闭；请使用公开网页读取或用户明确要求的浏览器操作。', 'WEB_BROWSER_READ_DISABLED', 409);
      if (!this.webSearch) this.webSearch = new WebSearchTool(this);
      const webKind = isPublicFetchTool(descriptor) ? 'page' : isPublicSearchTool(descriptor) ? 'query' : null;
      // Effects and unknown operations invalidate observations before their execution or approval.
      // 有副作用和未知操作在执行或审批前使观察缓存失效。
      if (!canRunInParallel(call)) this.observationCaches.get(context)?.clear();
      if (descriptor.source === 'builtin') validateBuiltinInput(descriptor, call.arguments);
      let path;
      let sensitiveRead = false;
      if (call.name.startsWith('filesystem.')) {
        const reading = filesystemReadTools.has(call.name);
        const target = resolveToolPath(context, call.arguments, [], { deferScopeCheck: true });
        // Check protected lexical destinations before touching a missing target, then recheck the bound real path.
        // 在访问尚未存在的目标前先检查其受保护位置，绑定真实路径后再次核验。
        const requestedManagedPath = context.managedWorkspace && within(context.workspaceRoot, target.path);
        if (!reading && (this.storageBoundary.isReadOnlyExtension(target.path) ||
            (!requestedManagedPath && this.storageBoundary.isOwned(target.path)) ||
            (this.skills.bundledDirectory && this.storageBoundary.aliases(target.path).some(alias => within(this.skills.bundledDirectory, alias)))))
          throw toolFailure('正式应用数据和内置技能由专用服务管理，文件工具不能改写。', 'PROTECTED_APP_DATA', 403);
        pathBinding = await bindLocalPath(target.path, { workspaceRoot: context.workspaceDiagnostic ? undefined : context.workspaceRoot,
          allowMissing: ['filesystem.write', 'filesystem.mkdir'].includes(call.name), allowHardLinks: reading });
        path = pathBinding.path; outsideWorkspace = pathBinding.outsideWorkspace;
        if (outsideWorkspace && !call.arguments.reason?.trim())
          throw toolFailure('访问工作范围以外的真实路径必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
        sensitiveRead = reading && call.name !== 'filesystem.list' && isSensitiveFilePath(path);
        const managedPath = context.managedWorkspace && pathBinding.workspaceRoot && within(pathBinding.workspaceRoot, path);
        if (this.storageBoundary.aliases(path).some(alias => this.workspaces.isControlPath(alias)))
          throw toolFailure('聊天工具目录的归属信息由应用管理。', 'PROTECTED_APP_DATA', 403);
        if (!reading && (this.storageBoundary.isReadOnlyExtension(path) ||
            (!managedPath && this.storageBoundary.isOwned(path)) ||
            (this.skills.bundledDirectory && this.storageBoundary.aliases(path).some(alias => within(this.skills.bundledDirectory, alias)))))
          throw toolFailure('正式应用数据和内置技能由专用服务管理，文件工具不能改写。', 'PROTECTED_APP_DATA', 403);
        if (reading && this.storageBoundary.isCredential(path))
          throw toolFailure('模型或工具连接和备份可能含密钥，文件工具不能读取。', 'PROTECTED_MODEL_CREDENTIALS', 403);
        if (reading && this.storageBoundary.isPrivateResult(path))
          throw toolFailure('完整工具结果须使用专用公开投影读取。', 'PROTECTED_TOOL_RESULT', 403);
        if (reading && this.storageBoundary.isOwned(path) && !managedPath) {
          outsideWorkspace = true;
          if (typeof call.arguments.reason !== 'string' || !call.arguments.reason.trim())
            throw toolFailure('读取正式应用数据必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
        }
        const existingTarget = await inspectLocalPath(path, { allowMissing: ['filesystem.write', 'filesystem.mkdir'].includes(call.name), allowHardLinks: reading });
        if (existingTarget && this.storageBoundary.aliases(await realpath(path)).some(alias => this.workspaces.isControlPath(alias)))
          throw toolFailure('聊天工具目录的归属信息由应用管理。', 'PROTECTED_APP_DATA', 403);
      } else if (call.name === 'web.fetch') {
        outsideWorkspace = true;
        validatePublicWebUrl(call.arguments.url);
        if (!call.arguments.reason.trim()) throw toolFailure('读取外部网页必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
      } else if (descriptor.source.startsWith('mcp:')) {
        outsideWorkspace = true;
        const reason = call.arguments.policy?.reason;
        if (typeof reason !== 'string' || !reason.trim() || reason.length > 2000)
          throw toolFailure('调用外部 MCP 服务必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
        await this.mcp.validateExecution(descriptor, call.arguments);
        call.arguments = await this.mcp.prepareBrowserExecution(descriptor, call.arguments,
          { sessionId: context.conversationId, ...context.browserInteraction });
      } else if (call.name === 'terminal.host.run' || call.name === 'terminal.host.start') {
        outsideWorkspace = true;
        if (!call.arguments.reason?.trim()) throw toolFailure('本机终端操作必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
        if (!this.hostTerminalRunner?.run || context.hostTerminalCapabilities.available !== true ||
            context.hostTerminalCapabilities.boundary !== 'host-terminal' || !context.hostTerminalCapabilities.shells?.includes(call.arguments.shell))
          throw toolFailure('当前本机终端不可用。', 'HOST_TERMINAL_UNAVAILABLE', 503);
        if (call.name === 'terminal.host.start' && context.hostTerminalCapabilities.backgroundJobs !== true)
          throw toolFailure('当前本机助手尚不支持后台终端任务。', 'HOST_TERMINAL_BACKGROUND_UNAVAILABLE', 503);
        if (call.arguments.visible === true && (context.hostTerminalCapabilities.protocolVersion !== 2 ||
            context.hostTerminalCapabilities.visibleTerminal !== true))
          throw toolFailure('当前原生助手不支持可见终端，请更新后重试。', 'HOST_TERMINAL_VISIBLE_UNAVAILABLE', 503);
        if (call.arguments.visible !== true && call.arguments.keepOpenMs !== undefined)
          throw toolFailure('窗口保留时间仅适用于可见终端。', 'HOST_TERMINAL_INVALID_REQUEST');
        if (call.arguments.visible === true && isExplicitForegroundForbidden(context.message, { kind: 'terminal' }))
          throw toolFailure('用户要求保持后台，不能打开可见终端窗口。', 'DESKTOP_FOREGROUND_FORBIDDEN', 403);
        path = call.arguments.cwd ?? context.workspaceRoot;
        if (!isAbsolute(path) || /^\\\\/.test(path))
          throw toolFailure('本机终端需要有效的绝对本地工作目录。', 'HOST_TERMINAL_INVALID_WORKSPACE');
        pathBinding = await bindLocalPath(path, { workspaceRoot: context.workspaceDiagnostic ? undefined : context.workspaceRoot });
        path = pathBinding.path;
        if (!(await revalidateLocalPathBinding(pathBinding)).isDirectory())
          throw toolFailure('本机终端需要有效的绝对本地工作目录。', 'HOST_TERMINAL_INVALID_WORKSPACE');
      } else if (call.name === 'terminal.host.read' || call.name === 'terminal.host.stop') {
        if (!this.hostTerminalJobs) throw toolFailure('当前本机终端不可用。', 'HOST_TERMINAL_UNAVAILABLE', 503);
        // Reads remain scoped to this chat; stopping a host process is a separately approved effect.
        // 读取仅限当前聊天，停止宿主进程属于单独审批的有副作用操作。
        outsideWorkspace = call.name === 'terminal.host.stop';
      } else if (call.name.startsWith('computer.')) {
        outsideWorkspace = true;
        if (!call.arguments.reason?.trim()) throw toolFailure('本机桌面操作必须说明原因。', 'OUTSIDE_WORKSPACE_REASON_REQUIRED', 403);
        const action = call.name.slice('computer.'.length);
        // Normalize before approval so the user approves the exact launch mode sent to the native host.
        // 审批前规范化，让用户批准的启动模式与发给原生宿主的完全一致。
        if (action === 'launch') {
          assertBrowserLaunchAllowed(context, call.arguments.appPath);
          pathBinding = await bindLocalPath(call.arguments.appPath, { allowHardLinks: true });
          call.arguments.appPath = pathBinding.path;
          assertBrowserLaunchAllowed(context, call.arguments.appPath);
          call.arguments = prepareDesktopLaunchArguments(call.arguments,
            { allowForeground: context.browserInteraction.background === false && context.browserInteraction.allowForeground === true });
          // Added defaults must obey the same bounds as model-supplied arguments.
          // 新增默认值遵守与模型传入参数相同的限制。
          validateBuiltinInput(descriptor, call.arguments);
        }
        if (action === 'launch' && isExplicitForegroundForbidden(context.message, {
            kind: isBrowserApplicationPath(call.arguments.appPath) ? 'browser' : 'application', appPath: call.arguments.appPath })) {
            if (call.arguments.background === false)
              throw toolFailure('用户要求保持后台，不能使用前台启动。', 'DESKTOP_FOREGROUND_FORBIDDEN', 403);
            call.arguments.background = true;
        }
        if (!this.desktopRunner?.run || context.desktopCapabilities.available !== true ||
            context.desktopCapabilities.boundary !== 'host-desktop' || !context.desktopCapabilities.operations?.includes(action))
          throw toolFailure('当前本机桌面操作不可用。', 'DESKTOP_UNAVAILABLE', 503);
      }
      const sandbox = context.sandboxCapabilities;
      const verifiedSandbox = sandbox.available === true && sandbox.sandbox === 'appcontainer' && sandbox.failClosed === true && sandbox.checksChildToken === true;
      if (call.name === 'terminal.run' || call.name === 'skill.run') {
        if (context.workspaceDiagnostic)
          throw toolFailure('关联工作文件夹暂时不可用，请更新挂载路径；其他能力可以继续。', 'WORKSPACE_UNAVAILABLE', 409);
        if (context.isolatedWorkspace) await this.workspaces.verify(context.conversationId, context.workspaceRoot);
        if ((!context.managedWorkspace && this.storageBoundary.isOwned(context.workspaceRoot)) ||
            this.storageBoundary.isManagedExtension(context.workspaceRoot))
          throw toolFailure('正式应用数据不能作为终端工作范围。', 'PROTECTED_APP_DATA', 403);
        if (!this.sandboxRunner?.run || !verifiedSandbox) throw toolFailure('已验证的 AppContainer 沙箱不可用，未在宿主执行。', 'SANDBOX_UNAVAILABLE', 503);
        const command = call.name === 'skill.run' ? 'node' : call.arguments.command.replace(/\.exe$/, '');
        if (!Array.isArray(sandbox.commands) || !sandbox.commands.includes(command)) throw toolFailure('此命令未由已验证沙箱声明支持。', 'SANDBOX_COMMAND_UNSUPPORTED', 400);
        if (command === 'cmd' && (call.arguments.args.length !== 3 || call.arguments.args[0].toLowerCase() !== '/d' ||
            call.arguments.args[1].toLowerCase() !== '/c' || !call.arguments.args[2].trim()))
          throw toolFailure('cmd 必须使用 /d /c 和一条命令文本。', 'INVALID_TOOL_ARGUMENTS');
        if (call.name === 'skill.run') {
          if (!supportsSkillExecution(sandbox))
            throw toolFailure('当前沙箱助手尚不支持经验证的技能包执行。', 'SANDBOX_SKILL_UNSUPPORTED', 503);
          if (!this.sandboxRunner.runSkill) throw toolFailure('技能沙箱执行器不可用。', 'SANDBOX_UNAVAILABLE', 503);
          preparedSkill = await this.skills.prepareScript(call.arguments.id, call.arguments.path, context, await this.getConfig(), { signal });
          if (!preparedSkill.environment.canRun)
            throw toolFailure('技能运行环境或依赖尚未满足，请先查看 skill.check 的诊断。', 'APP_SKILL_ENVIRONMENT_UNAVAILABLE', 409);
        }
      }
      if (webKind) webStage = await this.webSearch.check(context, webKind);
      if (needsToolApproval(context, call.name, { outsideWorkspace, verifiedSandbox, sensitiveRead })) {
        if (!interactive || typeof emit !== 'function') throw toolFailure('此工具需要交互审批，本次没有执行。', 'TOOL_APPROVAL_REQUIRED', 403);
        const approvalStarted = performance.now();
        let approved;
        try { approved = await this.approvals.wait(context, call, { signal, emit, outsideWorkspace,
          ...(sensitiveRead ? { summary: '读取可能含密钥的文件，批准后内容可能进入工具记录并发送给当前模型。' } : {}) }); }
        finally {
          const waited = Math.max(0, Math.ceil(performance.now() - approvalStarted));
          if (webStage) webStage.stage.started += waited;
          onApprovalWait(waited);
        }
        if (!approved) throw toolFailure('用户拒绝了此工具调用。', 'TOOL_DENIED', 403);
      }
      signal?.throwIfAborted();
      if (webStage) {
        const remainingMs = webStage.stage.durationMs - (performance.now() - webStage.stage.started);
        if (remainingMs <= 0) throw toolFailure('网页检索预算已用完。', 'WEB_STAGE_BUDGET_EXHAUSTED', 409);
        const durationMs = Math.max(1, Math.ceil(remainingMs));
        webDeadline = AbortSignal.timeout(durationMs);
        signal = signal ? AbortSignal.any([signal, webDeadline]) : webDeadline;
      }
      await this._assertOwnership(context);
      await this._assertToolConfiguration(context, descriptor, call.arguments);
      if (pathBinding) await revalidateLocalPathBinding(pathBinding);
      if (call.name.startsWith('computer.')) await this._assertDesktopForegroundPolicy(context, call, signal);
      let observationConnection;
      if (canReuseObservation(call) && descriptor.source.startsWith('mcp:'))
        observationConnection = (await this.mcp.validateExecution(descriptor, call.arguments)).connection;
      const cached = this.observationCaches.get(context)?.get(call, observationConnection);
      if (cached && descriptor.source.startsWith('mcp:')) {
        signal?.throwIfAborted();
        await this._assertOwnership(context);
        await this._assertToolConfiguration(context, descriptor, call.arguments);
        signal?.throwIfAborted();
        // Bind a fresh archive reference to this call; the previous call's reference is never reassigned.
        // 为当前调用绑定新的结果引用，不重新分配上一次调用的引用。
        const finished = await this._finishResult(context, call, cached.result);
        return { ...finished, reused: true, observationCapturedAt: cached.capturedAt,
          content: `[KYNXA_OBSERVATION_REUSED] Reused this request's successful observation captured at ${cached.capturedAt}; no new network request.\n\n${finished.content}` };
      }
      if (webKind) webStage = await this.webSearch.take(context, webKind);
      let result;
      executionStarted = true;
      if (call.name.startsWith('filesystem.')) result = await executeFilesystem(call.name, context, call.arguments, path, signal,
        { pathBinding, protectedRoots: outsideWorkspace || context.managedWorkspace ? [] :
            [this.root, this.dataHome, this.extensionRoot].flatMap(root => this.storageBoundary.aliases(root)),
          // A generic search approval never authorizes nested credential files; only explicit sensitive targets do.
          // 普通目录搜索的批准不授权读取其中的敏感文件，只有明确敏感目标的本次审批允许读取。
          denyRead: candidate => this.storageBoundary.isCredential(candidate) || this.storageBoundary.isPrivateResult(candidate) ||
            this.storageBoundary.aliases(candidate).some(alias => this.workspaces.isControlPath(alias)) ||
            (context.permissionMode !== 'full' && !sensitiveRead && isSensitiveFilePath(candidate)) });
      else if (call.name === 'web.fetch') return await this._finishResult(context, call, await this.webFetcher.run(call.arguments, signal));
      else if (call.name === 'web.search') {
        if (![...snapshot.descriptors.values()].some(isPublicSearchTool)) {
          const settings = this.retrieval ? await this.retrieval.effective(context.projectId) : null;
          const providers = (snapshot.mcpDiscovery ?? []).filter(server => (/(?:exa|brave)/i.test(server.id + ' ' + server.description) ||
            server.tools.some(isPublicSearchTool)) &&
            (!settings?.web.providerId || settings.web.providerId === 'auto' || settings.web.providerId === server.id));
          if (providers.length) snapshot = await this._connectRequestedServers(context, [providers[0].id]);
        }
        return await this._finishResult(context, call,
          await this.webSearch.run(context, call.arguments, { signal, emit, interactive, onApprovalWait }));
      }
      else if (call.name.startsWith('knowledge.')) {
        if (!this.retrieval) throw toolFailure('本地检索不可用。', 'RETRIEVAL_UNAVAILABLE', 503);
        if (call.name === 'knowledge.search') {
          const remainingTokens = this.evidenceBudgets.get(context);
          const argumentsForBudget = remainingTokens === undefined ? call.arguments : { ...call.arguments,
            maximumTokens: Math.min(call.arguments.maximumTokens ?? 32768, remainingTokens) };
          result = await this.retrieval.search(context, argumentsForBudget, { signal, modelReferences: true });
          if (result.budget?.audit) {
            result.budget.audit.modelRequested = { limit: call.arguments.limit ?? null,
              maximumTokens: call.arguments.maximumTokens ?? null };
            result.budget.audit.availableContextTokens = remainingTokens ?? null;
            if (remainingTokens !== undefined && (call.arguments.maximumTokens ?? result.budget.audit.configured.maximumTokens) > remainingTokens)
              result.budget.audit.earlyCutReasons.push({ reason: 'remaining-context', field: 'maximumTokens',
                proposed: call.arguments.maximumTokens ?? result.budget.audit.configured.maximumTokens,
                approved: result.budget.maximumTokens, unit: 'tokens' });
          }
          return await this._finishResult(context, call, { value: result, isError: false },
            { archiveId: result.evidenceArchiveId, modelProjection: id =>
              projectEvidenceSearchResult(publicToolResult({ content: [], structuredContent: result, isError: false }), id).structuredContent });
        }
        const operation = { 'knowledge.read': 'read', 'knowledge.relations': 'relations',
          'knowledge.assess': 'assess', 'knowledge.experience': 'experience' }[call.name];
        if (!operation) throw toolFailure('未知检索操作。', 'MODEL_TOOL_UNAVAILABLE', 400);
        result = await this.retrieval[operation](context, call.arguments, { signal });
      }
      else if (call.name === 'tool.search') {
        // Discovery may recover a changed/failed connection once; never replay a business operation.
        // 目录发现可有界恢复一次变化或失败连接，不重放任何业务操作。
        snapshot = await this._refreshDiscoveryOnce(context, snapshot);
        const discoveryConfig = this.liveConfig ?? snapshot.config;
        const discoveryHeaders = this.mcp.discovery({ ...discoveryConfig,
          mcpServers: discoveryConfig.mcpServers.filter(server => canUseBrowserServer(context, server)) }, context);
        const discovered = [...(snapshot?.descriptors.values() ?? [])], existing = new Set(discovered.map(tool => tool.name));
        for (const server of discoveryHeaders) for (const tool of server.tools)
          if (!existing.has(tool.name)) { existing.add(tool.name); discovered.push(tool); }
        const all = searchTools(discovered.filter(tool =>
          isConfiguredToolEnabled(discoveryConfig, tool) && !hasToolConfigurationChanged(snapshot.config, discoveryConfig, tool) &&
            this.webSearch.available(context, tool)), call.arguments.query ?? '');
        const offset = boundedInteger(call.arguments.offset, 0, 0, 100000);
        const limit = boundedInteger(call.arguments.limit, 10, 1, 20);
        const serverHeads = searchTools(discoveryHeaders.map(({ tools, ...server }) => server), call.arguments.query ?? '');
        result = { tools: all.slice(offset, offset + limit).map(tool => ({ ...publicDescriptor(tool),
          schemaState: tool.discoveryOnly ? 'needs-connection' : snapshot.model?.selected.some(item => item.name === tool.name) ? 'loaded' : 'deferred',
          ...(tool.discoveryOnly ? { catalogVerified: false, executable: false } : {}) })), offset,
          nextOffset: Math.min(all.length, offset + limit), total: all.length, hasMore: offset + limit < all.length };
        if (serverHeads.length) Object.assign(result, { servers: serverHeads.slice(offset, offset + limit).map(server => ({ ...server,
          schemaState: 'service-header', loadName: server.name, executable: false })), serverTotal: serverHeads.length,
          serverNextOffset: Math.min(serverHeads.length, offset + limit), serverHasMore: offset + limit < serverHeads.length });
        if (toolSelectionSignals(call.arguments.query ?? '').deviceState || context.toolIntent?.deviceState)
          result.capabilityCheck = this.capabilitySnapshot(context);
      }
      else if (call.name === 'tool.load') {
        if (!snapshot?.model) throw toolFailure('当前请求没有模型工具预算。', 'TOOL_CATALOG_UNAVAILABLE', 409);
        call.arguments.names = [...new Set(snapshot.model.resolveNames(call.arguments.names))];
        const currentConfig = this.liveConfig ?? snapshot.config;
        const currentHeaders = this.mcp.discovery({ ...currentConfig,
          mcpServers: currentConfig.mcpServers.filter(server => canUseBrowserServer(context, server)) }, context);
        const requestedServers = [], requestedHeads = [];
        for (const name of call.arguments.names) {
          const existing = snapshot.descriptors.get(name);
          if (existing && !isConfiguredToolEnabled(currentConfig, existing)) continue;
          const server = currentHeaders.find(item => item.name === name || item.tools.some(tool => tool.name === name) || existing?.serverId === item.id);
          const ready = existing && this.mcp.readyConnections.get(existing.key);
          const liveDescriptor = ready && !ready.closed && ready.tools.find(tool => tool.name === name);
          // A retained request schema is not a live connection or current directory.
          // Rebind just this service after disconnect/schema change; never replay its calls.
          // 请求保留的 schema 不代表连接就绪或目录仍有效；断连或 schema 变化仅重新绑定该服务，不重放业务调用。
          const liveSchemaMatches = liveDescriptor && liveDescriptor.operation === existing.operation && liveDescriptor.toolName === existing.toolName &&
            isDeepStrictEqual(liveDescriptor.inputSchema, existing.inputSchema) && isDeepStrictEqual(liveDescriptor.originalInputSchema, existing.originalInputSchema);
          if (!server || liveSchemaMatches && !hasToolConfigurationChanged(snapshot.config, currentConfig, existing)) continue;
          requestedServers.push(server.id);
          if (server.name === name) requestedHeads.push(name);
        }
        if (requestedServers.length) snapshot = await this._connectRequestedServers(context, requestedServers);
        const availableNames = [], unavailable = [];
        for (const name of call.arguments.names) {
          if (requestedHeads.includes(name)) {
            const serverId = name.slice('mcp.'.length);
            const tools = [...snapshot.descriptors.values()].filter(tool => tool.serverId === serverId);
            const beforeCount = availableNames.length;
            if (!tools.length) unavailable.push({ name, code: this.mcp.errors.get(serverId) ?? 'MCP_NO_ENABLED_TOOLS' });
            // A service can expose more schemas than this model accepts. Load a
            // bounded real subset; all remaining verified names stay searchable.
            // 服务 schema 可超过当前模型额度；仅装入预算容纳的真实子集，其余已核验工具仍可发现。
            for (const tool of tools) {
              if (availableNames.includes(tool.name) || !this.webSearch.available(context, tool)) continue;
              const trial = [...availableNames, tool.name];
              const keep = [...snapshot.descriptors.values()].filter(item => ['tool.search', 'tool.load', 'tool.result.read'].includes(item.name));
              if (snapshot.model.fits([...keep, ...trial.map(id => snapshot.descriptors.get(id))])) availableNames.push(tool.name);
            }
            if (tools.length && availableNames.length === beforeCount)
              unavailable.push({ name, code: tools.every(tool => !this.webSearch.available(context, tool))
                ? 'WEB_STAGE_BUDGET_EXHAUSTED' : 'TOOL_CATALOG_BUDGET' });
            continue;
          }
          const tool = snapshot.descriptors.get(name);
          if (!tool || !isConfiguredToolEnabled(this.liveConfig ?? snapshot.config, tool)) {
            const revoked = snapshot.revokedNames?.has(name) || tool && hasToolConfigurationChanged(snapshot.config, this.liveConfig ?? snapshot.config, tool);
            unavailable.push({ name, code: revoked ? 'AGENT_CONFIG_CHANGED' : 'TOOL_NOT_FOUND',
              ...(revoked ? { executed: false, recoverable: true } : {}) });
          }
          else if (!this.webSearch.available(context, tool)) unavailable.push({ name, code: 'WEB_STAGE_BUDGET_EXHAUSTED' });
          else if (!availableNames.includes(name)) availableNames.push(name);
        }
        if (!availableNames.length) {
          const code = unavailable.some(item => item.code === 'WEB_STAGE_BUDGET_EXHAUSTED') ? 'WEB_STAGE_BUDGET_EXHAUSTED' :
            unavailable.some(item => item.code === 'TOOL_CATALOG_BUDGET') ? 'TOOL_CATALOG_BUDGET' :
              unavailable.some(item => item.code === 'AGENT_CONFIG_CHANGED') ? 'AGENT_CONFIG_CHANGED' : 'TOOL_NOT_FOUND';
          throw Object.assign(toolFailure('请求的工具当前不可用，请发现其他能力。', code, 409),
            code === 'AGENT_CONFIG_CHANGED' ? { toolConfigurationRevoked: true } : {});
        }
        result = { ...snapshot.model.load(availableNames), ...(unavailable.length ? { unavailable } : {}) };
      }
      else if (call.name === 'tool.result.read') result = await (this.results.readModel ?? this.results.read).call(
        this.results, context, call.arguments.id, { offset: call.arguments.offset, limit: call.arguments.limit });
      else if (call.name.startsWith('conversation.history.'))
        result = await executeHistoryTool(this.conversations, context, call.name, call.arguments, signal);
      else if (call.name === 'skill.list') {
        const available = await this.listSkills(context), offset = boundedInteger(call.arguments.offset, 0, 0, 128);
        const limit = boundedInteger(call.arguments.limit, 32, 1, 128), page = [];
        for (const skill of available.slice(offset, offset + limit)) {
          if (JSON.stringify([...page, skill]).length > MAX_TOOL_RESULT_CHARS - 1000) break;
          page.push(skill);
        }
        result = { skills: page, offset, nextOffset: offset + page.length, hasMore: offset + page.length < available.length,
          total: available.length, discovery: this.skills.discovery.get(available) };
      }
      else if (call.name === 'skill.read') result = await this.readSkill(call.arguments.id, context);
      else if (call.name === 'skill.resource.read') result = await this.skills.readResource(call.arguments.id, call.arguments.path,
        context, await this.getConfig(), { signal, offset: call.arguments.offset, limit: call.arguments.limit });
      else if (call.name === 'skill.inspect') result = await this.skills.inspect(call.arguments.id, context, await this.getConfig(), { signal });
      else if (call.name === 'skill.check') result = await this.skills.checkEnvironment(call.arguments.id, context, await this.getConfig(),
        { signal, sandboxCapabilities: context.sandboxCapabilities });
      else if (call.name === 'terminal.run' || call.name === 'skill.run') {
        if (!this.sandboxRunner?.run || !verifiedSandbox) throw toolFailure('已验证的 AppContainer 沙箱不可用，未在宿主执行。', 'SANDBOX_UNAVAILABLE', 503);
        const request = { workspaceRoot: context.workspaceRoot, command: call.arguments.command, args: call.arguments.args,
          timeoutMs: boundedInteger(call.arguments.timeoutMs, 30000, 100, 120000), trustedManagedWorkspace: context.managedWorkspace };
        const response = call.name === 'skill.run' ? await this.sandboxRunner.runSkill({ ...request, package: preparedSkill }, signal)
          : await this.sandboxRunner.run(request, signal);
        if (call.name === 'skill.run' && !verifiesSkillExecution(response, preparedSkill))
          throw toolFailure('技能执行结果缺少完整的包验证记录。', 'SANDBOX_INVALID_RESULT', 500);
        if (response.sandbox !== 'appcontainer') throw toolFailure('执行结果缺少真实 AppContainer 证明。', 'SANDBOX_INVALID_RESULT', 500);
        if (typeof response.stagingDirectory === 'string') this.stages.get(context)?.add(response.stagingDirectory);
        return await this._finishResult(context, call, { value: response,
          isError: response.exitCode !== 0 || response.timedOut === true || response.cancelled === true,
          code: response.cancelled ? 'TOOL_CANCELLED' : undefined, sandbox: 'appcontainer', outsideWorkspace: false });
      } else if (call.name === 'terminal.host.start') {
        result = await this.hostTerminalJobs.start(context, { shell: call.arguments.shell, script: call.arguments.script,
          cwd: path, pathBinding, timeoutMs: call.arguments.timeoutMs }, signal);
      } else if (call.name === 'terminal.host.read') {
        result = this.hostTerminalJobs.read(context, call.arguments);
      } else if (call.name === 'terminal.host.stop') {
        result = await this.hostTerminalJobs.stop(context, call.arguments, signal);
      } else if (call.name === 'terminal.host.run') {
        return await this._finishResult(context, call, { ...await this.hostTerminalRunner.run({
          shell: call.arguments.shell, script: call.arguments.script, cwd: path, pathBinding,
          ...(call.arguments.visible !== undefined ? { visible: call.arguments.visible } : {}),
          ...(call.arguments.keepOpenMs !== undefined ? { keepOpenMs: call.arguments.keepOpenMs } : {}),
          timeoutMs: boundedInteger(call.arguments.timeoutMs, 30000, 100, 120000) }, signal,
          output => emit?.({ type: 'terminal_output', terminal: { toolCallId: call.id, ...output } })), outsideWorkspace: true });
      } else if (call.name.startsWith('computer.')) {
        return await this._finishResult(context, call, { ...await this.desktopRunner.run(call.name.slice('computer.'.length), call.arguments, signal), outsideWorkspace: true });
      } else {
        const observed = await runResourceTask(this.resources, { taskId: `mcp:${call.id}`, workspaceId: context.projectId ?? context.conversationId,
          kind: 'foreground', cpuThreads: 1, memoryBytes: 32 * 1024 * 1024 },
          () => {
            // Admission failures precede dispatch; they cannot leave an unknown external mutation.
            // 资源准入失败发生在派发之前，不能误报成外部变更结果未知。
            mcpDispatchAttempted = true;
            return this.mcp.execute(descriptor, call.arguments, signal,
              { sessionId: context.conversationId, ...context.browserInteraction });
          }, { signal });
        const finished = await this._finishResult(context, call, observed);
        if (canReuseObservation(call) && !finished.isError && !finished.code) {
          let cache = this.observationCaches.get(context);
          if (!cache) { cache = new RequestObservationCache(); this.observationCaches.set(context, cache); }
          if (isConfiguredToolEnabled(this.liveConfig ?? snapshot.config, descriptor)) cache.remember(call, observed, observationConnection);
        }
        return finished;
      }
      return await this._finishResult(context, call, { value: result, isError: false, outsideWorkspace });
    } catch (error) {
      const callName = typeof call?.name === 'string' ? call.name : '';
      const webExpired = webDeadline?.aborted && !originalSignal?.aborted;
      const cancelled = !webExpired && (signal?.aborted || error?.name === 'AbortError');
      const desktopTimedOut = callName.startsWith('computer.') && ['DESKTOP_TIMED_OUT', 'DESKTOP_TIMEOUT', 'DESKTOP_READ_TIMEOUT'].includes(error?.code);
      const externalOutcomeLost = callName.startsWith('mcp.') && ['MCP_TIMEOUT', 'MCP_CONNECTION_LOST'].includes(error?.code);
      if (['terminal.run', 'skill.run'].includes(callName) && error?.sandboxResult?.protocolVersion === 1 &&
          error.sandboxResult.sandbox === 'appcontainer' && error.sandboxResult.tokenVerified === true &&
          error.sandboxResult.workspaceCopy === true && error.sandboxResult.activeProcessesAfterExit === 0 &&
          typeof error.sandboxResult.cancelled === 'boolean' && typeof error.sandboxResult.timedOut === 'boolean' &&
          typeof error.sandboxResult.stdout === 'string' && typeof error.sandboxResult.stderr === 'string' &&
          Number.isInteger(error.sandboxResult.exitCode) && (callName !== 'skill.run' || verifiesSkillExecution(error.sandboxResult, preparedSkill))) {
        const partial = error.sandboxResult;
        // SandboxRunner owns cancellation cleanup, including native completion just before stop.
        // SandboxRunner 负责取消后的清理，包括停止前刚完成的原生操作。
        const interrupted = partial.cancelled === true || partial.timedOut === true;
        return this._finishResult(context, call, { value: partial,
          isError: interrupted || partial.exitCode !== 0,
          code: partial.cancelled === true ? 'TOOL_CANCELLED' : partial.timedOut === true ? 'TOOL_TIMED_OUT' : undefined,
          sandbox: 'appcontainer' });
      }
      const code = webExpired ? 'WEB_STAGE_BUDGET_EXHAUSTED' : cancelled ? 'TOOL_CANCELLED' : safeErrorCode(error);
      let message = webExpired ? '网页检索阶段已到期，已有证据与其他代码任务可以继续。' : cancelled ? '工具调用已取消。' : typeof error?.message === 'string' ? error.message : '工具执行失败。';
      if (!cancelled && !webExpired && error?.webFailureReason === 'dns-non-public') {
        const snapshot = this.catalogs.get(context);
        const hasBackgroundBrowser = error.allowBackgroundReadHint === true && [...(snapshot?.descriptors.values() ?? [])].some(tool =>
          /(?:take_snapshot|evaluate_script|browser_snapshot|browser_evaluate)$/u.test(tool.toolName ?? '') &&
          isBackgroundBrowserConnection(snapshot.servers?.get(tool.serverId)));
        message = '当前 DNS 答案包含无效或非公共地址，可能来自代理的虚拟 IP；本次 HTTP 读取已拒绝。' +
          (hasBackgroundBrowser ? '可以发现并加载已允许的后台网页读取工具，不会打开本机浏览器窗口。'
            : '请使用已有搜索证据或说明读取阻碍，不会自动打开本机浏览器。');
      }
      const mayHaveEffect = (callName.startsWith('computer.') && !isDesktopObservation(callName)) ||
        callName.startsWith('mcp.') || ['terminal.run', 'terminal.host.run', 'terminal.host.start', 'terminal.host.stop', 'skill.run'].includes(callName) ||
        (callName.startsWith('filesystem.') && !filesystemReadTools.has(callName));
      const notDispatched = !executionStarted || callName.startsWith('mcp.') &&
        (!mcpDispatchAttempted || isMcpExecutionNotDispatched(error));
      const unknown = !webExpired && !notDispatched && mayHaveEffect &&
        (cancelled || desktopTimedOut || externalOutcomeLost || error?.outcomeUnknown === true);
      // Failed observations have no write outcome to verify. Archive the failure just like a returned receipt,
      // so the next model turn can change approach without losing the call/result pair.
      // 失败观察没有待核验的写入结果，但仍像正常回执一样归档，使下一轮可调整方法且保留调用和结果配对。
      if (executionStarted && (callName.startsWith('computer.') || callName.startsWith('mcp.')))
        return this._finishResult(context, call, { value: { completed: false, error: { code, message: boundedContent(message) },
          ...(notDispatched ? { executed: false } : {}),
          outcome: unknown ? 'unknown' : cancelled ? 'cancelled' : 'failed' }, isError: true, code,
          ...(notDispatched ? { executed: false } : {}), ...(unknown ? { status: 'unknown' } : {}), outsideWorkspace });
      const job = unknown && callName === 'terminal.host.start' ? this.hostTerminalJobs?.jobs.get(error.jobId) : null;
      const recoveryHandle = job && job.conversationId === context.conversationId
        ? { kind: 'host-terminal-job', jobId: job.id } : null;
      let modelMessage = callName.startsWith('knowledge.') ? projectRetrievalModelView({ message }).message : message;
      if (recoveryHandle) modelMessage = `Host terminal job ${recoveryHandle.jobId}: query terminal.host.read with this jobId; do not replay the command. / 查询此后台任务状态，不能重新执行原命令。\n${modelMessage}`;
      return { content: boundedContent(modelMessage), isError: true, code, outsideWorkspace,
        executionEnvironment: this.executionEnvironmentFor(context, callName),
        ...(recoveryHandle ? { recoveryHandle } : {}),
        ...(notDispatched ? { executed: false } : {}),
        ...(error.toolConfigurationRevoked === true && (!executionStarted || callName === 'tool.load')
          ? { executed: false, recoverable: true } : {}),
        ...(unknown ? { status: 'unknown' } : {}) };
    }
  }

  async verifyUnknownEffects(context, records, observation) {
    if (observation.call.name !== 'terminal.host.read' || observation.result.isError) return [];
    const proofs = [];
    for (const { call, result } of records) {
      const handle = result.recoveryHandle;
      if (call.name !== 'terminal.host.start' || handle?.kind !== 'host-terminal-job' ||
          observation.call.arguments.jobId !== handle.jobId) continue;
      try {
        // Read the owned registry again, not the model-visible output or arguments claiming success.
        // 再读属于当前聊天的任务注册表，不能相信模型可见输出或参数自报的成功状态。
        const job = this.hostTerminalJobs.read(context, { jobId: handle.jobId, offset: 0, limit: 1 });
        const processId = job.processId ?? job.receipt?.processId;
        if (!Number.isSafeInteger(processId) || processId <= 0 ||
            !['running', 'completed', 'error', 'cancelled'].includes(job.status)) continue;
        const receipt = await this._finishResult(context, call, { value: { ...job, processId,
          dispatchConfirmed: true, verificationToolCallId: observation.call.id,
          taskSuccessCertified: false }, isError: false });
        proofs.push({ toolCallId: call.id, observationToolCallId: observation.call.id,
          outcome: 'dispatch-confirmed', result: receipt });
      } catch { /* Unavailable, foreign or uncertain jobs stay unconfirmed. 不可用、其他聊天或未知任务继续保持未确认。 */ }
    }
    return proofs;
  }

  async _finishResult(context, call, result, { archiveId, modelProjection } = {}) {
    const executionEnvironment = this.executionEnvironmentFor(context, call.name);
    const canonical = result.canonical ?? { content: [], structuredContent: result.value,
      isError: Boolean(result.isError), ...(result.code ? { code: result.code } : {}) };
    const status = result.status === 'unknown' ? 'unknown' : result.code === 'TOOL_CANCELLED' ? 'cancelled' : result.isError ? 'error' : 'completed';
    let resultRef, storageError;
    try { resultRef = await this.results.save(context, call, canonical, { id: archiveId, executionEnvironment }); }
    catch (error) { storageError = { code: safeErrorCode(error, 'TOOL_RESULT_SAVE_FAILED'), saved: false }; }
    let content;
    if (storageError) {
      const unsavedValue = modelProjection && result.value?.items
        ? { ...result.value, evidenceArchiveId: undefined, items: result.value.items.map(({ modelSourceRef, ...item }) => item) }
        : result.value;
      content = previewToolResult({ status, storageError,
        output: result.canonical ? publicToolResult(canonical) : unsavedValue }, { status });
    } else if (modelProjection) {
      // Short references become visible only after their complete canonical archive is durable.
      // 完整 canonical 归档持久化后，模型才能看到对应短引用。
      content = previewToolResult(modelProjection(resultRef.id), { resultRef, status });
    } else if (result.canonical) {
      content = result.content.length <= MAX_TOOL_RESULT_CHARS ? result.content
        : previewToolResult(publicToolResult(canonical, { resultRef }), { resultRef, status });
    } else content = previewToolResult(result.value, { resultRef, status });
    // Every protocol shares this projection boundary; preserve raw paths only in the durable/local receipt.
    // 三种模型协议共用此投影边界，原始路径只保留在正式归档和本地回执中。
    if (call.name.startsWith('knowledge.')) {
      try { content = JSON.stringify(projectRetrievalModelView(JSON.parse(content))); }
      catch { content = projectRetrievalModelView({ message: content }).message; }
    }
    // Archive IDs and retry diagnostics change on every call, even when retrieved evidence is identical.
    // 每次调用的归档 ID 和重试诊断都会变化，不能把这些变化当作检索取得了新证据。
    const observedResult = call.name === 'knowledge.search' && Array.isArray(result.value?.items)
      ? { strategy: result.value.strategy, items: result.value.items.map(({ modelSourceRef, ...item }) => item) }
      : publicToolResult(canonical);
    return { content, isError: Boolean(result.isError), ...(resultRef ? { resultRef } : {}), status, executionEnvironment,
      ...(result.executed === false ? { executed: false } : {}),
      observationHash: observationFingerprint(observedResult),
      ...(result.code || storageError ? { code: result.code ?? 'TOOL_RESULT_SAVE_FAILED' } : {}), ...(result.sandbox ? { sandbox: result.sandbox } : {}),
      ...(result.browser ? { browser: result.browser } : {}),
      outsideWorkspace: result.outsideWorkspace ?? false };
  }

  async releaseContext(context) {
    if (!context || !this.contexts.has(context)) return;
    this.approvals.cancelContext(context);
    const stages = this.stages.get(context);
    this.stages.delete(context);
    this.contexts.delete(context);
    this.catalogs.delete(context);
    this.observationCaches.delete(context);
    this.discoveryRefreshes.delete(context);
    if (this.sandboxRunner?.cleanup && stages) await Promise.allSettled([...stages].map(path => this.sandboxRunner.cleanup(path)));
  }

  async close() {
    if (!this.closure) {
      this.closed = true;
      this.approvals.close();
      this.closure = this._closeResources();
    }
    return this.closure;
  }

  async _closeResources() {
    // One failed owner must not prevent the remaining processes from being stopped.
    // Wait for all closures before cleaning snapshots, and retain the failure for runtime retirement.
    // 一个所有者清理失败不能阻止其余进程停止；先等待全部关闭，再清理快照，并保留失败用于运行时退役判断。
    const outcomes = await Promise.allSettled([
      () => this.desktopRunner?.close?.(),
      () => this.hostTerminalJobs?.close?.(),
      () => this.hostTerminalRunner?.close?.(),
      () => this.webFetcher?.close?.(),
      () => this.retrieval?.close?.(),
      () => this.mcp.close()
    ].map(close => Promise.resolve().then(close)));
    const cleanup = await Promise.allSettled([Promise.resolve().then(() => this.sandboxRunner?.cleanupAll?.())]);
    const failures = [...outcomes, ...cleanup].filter(outcome => outcome.status === 'rejected');
    if (failures.length) throw failures[0].reason;
  }
}
