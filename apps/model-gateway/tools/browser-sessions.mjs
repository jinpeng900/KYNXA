import { browserConnection } from './browser-connections.mjs';
import { toolFailure } from '../platform/tool-paths.mjs';
import { scriptRequestsBrowserForeground } from './browser-script-policy.mjs';

const chromeReads = new Set(['list_pages', 'take_snapshot', 'take_screenshot', 'list_console_messages',
  'get_console_message', 'list_network_requests', 'get_network_request']);
const playwrightReads = new Set(['browser_snapshot', 'browser_take_screenshot', 'browser_console_messages', 'browser_network_requests']);
const navigationTools = new Set(['new_page', 'navigate_page', 'browser_navigate', 'browser_navigate_back', 'browser_navigate_forward']);
const snapshotTools = new Set(['take_snapshot', 'browser_snapshot']);
const maximumSessions = 128, maximumTabs = 128;

export function currentBrowserInstructionText(trustedUserText) {
  return String(trustedUserText).slice(-8000).replace(/```[\s\S]*?```/g, '').replace(/^>.*$/gm, '');
}

/**
 * Explicit prohibition is distinct from the default preference for background browsing.
 * 用户明确禁止打开窗口，与默认偏好后台浏览是不同约束。
 */
export function isExplicitForegroundForbidden(trustedUserText = '', target) {
  const clauses = currentBrowserInstructionText(trustedUserText).split(/[，。；,.!?;\n]/u);
  return clauses.some(clause => {
    if (/(?:并)?不是(?:说)?(?:不能|不允许|禁止)|并非(?:不能|不允许|禁止)|不(?:代表|意味着)(?:不能|不允许)/u.test(clause)) return false;
    if (!/(?:不要|别|勿|禁止|不能|不允许)[^，。；,.!?;\n]{0,24}(?:抢.{0,3}焦点|激活|切.{0,3}前台|置.{0,3}前台)|保持.{0,3}后台|(?:仅|只能|始终).{0,3}后台|(?:do not|don't|never|without)[^,.!?;\n]{0,50}(?:focus|activat(?:e|ing)|bring.{0,40}(?:front|foreground))/i.test(clause)) return false;
    if (!target) return true;
    const browser = /浏览器|\bbrowser\b|chrome|chromium|edge|firefox|brave/iu;
    const terminal = /终端|命令行|powershell|\bcmd\b|\bterminal\b|控制台/iu;
    const name = String(target.appPath ?? target.applicationName ?? target.title ?? '').replaceAll('\\', '/').split('/').at(-1).replace(/\.exe$/iu, '');
    if (browser.test(clause)) {
      const specifiedBrands = clause.match(/chrome|chromium|edge|firefox|brave/giu);
      if (specifiedBrands?.length && !/浏览器|\bbrowser\b/iu.test(clause) && name)
        return specifiedBrands.some(brand => name.toLowerCase().includes(brand.toLowerCase()));
      return target.kind === 'browser' || browser.test(name);
    }
    if (terminal.test(clause)) return target.kind === 'terminal' || terminal.test(name);
    if (/记事本|notepad/iu.test(clause)) return /记事本|notepad/iu.test(name);
    if (/抢.{0,3}焦点|保持.{0,3}后台|(?:仅|只能|始终).{0,3}后台/u.test(clause)) return true;
    // Unqualified "do not activate" applies to this task; a named application
    // prohibition applies only to the supplied, observed application identity.
    // 未限定对象的禁止激活作用于当前任务；点名软件的限制仅匹配已观察的软件身份。
    if (/(?:激活|focus|activate)\s*(?:当前|这个|该)?\s*(?:窗口|软件|应用|window|application|app)\s*$/iu.test(clause)) return true;
    return !/(?:激活|focus|activate)\s*[^\s]{2,}/iu.test(clause) || (name && clause.toLowerCase().includes(name.toLowerCase()));
  });
}

export function hasTargetedForegroundRestriction(trustedUserText = '') {
  const text = currentBrowserInstructionText(trustedUserText);
  return isExplicitForegroundForbidden(text) && /浏览器|\bbrowser\b|chrome|edge|firefox|终端|powershell|\bcmd\b|terminal|记事本|notepad|(?:激活|focus|activate)\s*[^\s，。；,.!?;]{2,}/iu.test(text);
}

/**
 * The caller supplies current user instructions, never a tool reason or historical permission.
 * 调用方提供当前用户指令，不能用工具理由或历史权限替代。
 */
export function inferBrowserInteractionPolicy(trustedUserText = '') {
  const text = currentBrowserInstructionText(trustedUserText), forbid = isExplicitForegroundForbidden(trustedUserText, { kind: 'browser' });
  const foreground = !forbid && text.split(/[，。；,.!?;\n]/u).some(clause =>
    !/(?:不要|不必|无需|不能|别|do not|don't|without)/iu.test(clause) &&
    /(?:切换?到|切到|置于|放到|带到|显示在).{0,3}前台|激活.{0,8}(?:浏览器|窗口|Chrome|Edge)|(?:focus|activate).{0,15}(?:browser|window|chrome|edge)|bring.{0,15}(?:browser|window|chrome|edge|it).{0,15}(?:front|foreground)/i.test(clause));
  return { background: !foreground, allowForeground: !forbid };
}

export function browserOperation(descriptor, server) {
  if (descriptor.operation !== 'tools/call') return null;
  const connection = server ? browserConnection(server) : null;
  // Tool names and server descriptions are untrusted. A third-party "click" or
  // "take_snapshot" must not acquire a browser identity from its name alone.
  // 工具名称和服务说明不可信；第三方 click 或 take_snapshot 不能仅凭名称取得浏览器身份。
  const engine = connection?.engine;
  if (!engine) return null;
  return { engine, mode: connection?.mode ?? 'custom-browser', headless: connection?.headless === true,
    navigation: navigationTools.has(descriptor.toolName),
    readOnly: engine === 'chrome-devtools' ? chromeReads.has(descriptor.toolName)
      : playwrightReads.has(descriptor.toolName) };
}

/**
 * Identity diagnostics never expose the configured endpoint or URL credentials.
 * 身份诊断不暴露配置端点或 URL 中的凭据。
 */
export function publicBrowserUrl(value) {
  if (typeof value !== 'string' || value.length > 8192) return undefined;
  try {
    const url = new URL(value);
    if (!['http:', 'https:', 'about:'].includes(url.protocol)) return undefined;
    url.username = ''; url.password = ''; url.hash = '';
    for (const name of [...url.searchParams.keys()])
      if (/token|secret|password|credential|api[-_]?key|authorization|signature|^code$|^key$/i.test(name)) url.searchParams.delete(name);
    return url.href;
  } catch { return undefined; }
}

function references(args) {
  const refs = [];
  const visit = (value, depth = 0) => {
    if (!value || typeof value !== 'object' || depth > 8) return;
    for (const [key, child] of Object.entries(value)) {
      if (typeof child === 'string' && (/^(?:uid|\w+_uid|ref|\w+Ref)$/.test(key) ||
          (key === 'target' && /^(?:[ef]\d+(?:e\d+)?|\d+_\d+)$/.test(child)))) refs.push(child);
      else if (typeof child === 'object') visit(child, depth + 1);
    }
  };
  visit(args); return refs;
}

function resultEvidence(result) {
  const text = (result.content ?? []).filter(block => block.type === 'text').map(block => block.text).join('\n').slice(0, 256000);
  const tabs = [];
  for (const match of text.matchAll(/^(\d+):\s+([^\r\n]+)$/gm)) {
    // Chrome 1.10.1 emits "id: title (url) [selected]"; older receipts omit
    // the title. Parse only these live listing formats, never browser history.
    // 仅解析 Chrome 1.10.1 及旧版本实际返回的实时页面清单格式，不读取浏览器历史。
    const selected = /\s+\[selected\]$/.test(match[2]), label = match[2].replace(/\s+\[selected\]$/, '');
    const url = publicBrowserUrl(label.match(/\((https?:\/\/.*)\)$/)?.[1] ?? label);
    if (url) tabs.push({ tabId: match[1], url, selected });
  }
  for (const match of text.matchAll(/^-\s+(\d+):\s+(\(current\)\s+)?\[[^\r\n]*?\]\(([^\r\n]*)\)/gm)) {
    const url = publicBrowserUrl(match[3]);
    if (url) tabs.push({ tabId: match[1], url, selected: !!match[2] });
  }
  const refs = new Set();
  for (const match of text.matchAll(/\buid=([^\s]+)|\[ref=([^\]\s]+)\]/g)) {
    if (refs.size < 4096) refs.add(match[1] ?? match[2]);
  }
  const pageUrl = publicBrowserUrl(text.match(/^- Page URL:\s*(\S+)/m)?.[1] ??
    text.match(/RootWebArea[^\r\n]*\burl="([^"]+)"/)?.[1]);
  return { tabs, refs, pageUrl };
}

/**
 * Bounded, request-independent browser identities sourced only from live tool receipts.
 * 浏览器身份有容量限制、独立于请求，且只来自实时工具回执。
 */
export class BrowserSessionRegistry {
  constructor() { this.connections = new Map(); this.sessions = new Map(); }

  _state(descriptor, operation, sessionId) {
    let connection = this.connections.get(descriptor.key);
    if (!connection) {
      connection = { connectionId: descriptor.key, serverId: descriptor.serverId, ...operation, tabs: new Map(), selectedTabId: undefined, generation: 0 };
      this.connections.set(descriptor.key, connection);
    }
    const id = typeof sessionId === 'string' && sessionId.length <= 128 ? sessionId : descriptor.key;
    const sessionKey = descriptor.key + ':' + id;
    let session = this.sessions.get(sessionKey);
    if (!session) {
      if (this.sessions.size >= maximumSessions) this.sessions.delete(this.sessions.keys().next().value);
      session = { sessionId: id, connectionId: descriptor.key, selectedTabId: undefined, needsObservation: false };
      this.sessions.set(sessionKey, session);
    }
    return { connection, session };
  }

  prepare(descriptor, args, server, options = {}) {
    const operation = browserOperation(descriptor, server);
    if (!operation) return { args };
    operation.readOnly ||= descriptor.toolName === 'browser_tabs' && args.action === 'list';
    const { connection, session } = this._state(descriptor, operation, options.sessionId);
    const prepared = structuredClone(args), properties = descriptor.originalInputSchema?.properties ?? {};
    const allowForeground = options.allowForeground !== false;
    let background = options.background !== false;
    if (prepared.background === false || prepared.bringToFront === true) background = false;
    if (!allowForeground && !background)
      throw toolFailure('当前任务要求后台浏览，不能激活浏览器；请保持后台或由用户明确要求前台。', 'BROWSER_FOREGROUND_FORBIDDEN', 409);
    if (!allowForeground && ['evaluate_script', 'browser_evaluate', 'browser_run_code'].includes(descriptor.toolName) &&
        [prepared.function, prepared.code].some(value => typeof value === 'string' && scriptRequestsBrowserForeground(value)))
      throw toolFailure('后台任务不能通过脚本激活窗口或打开可能抢焦点的窗口。', 'BROWSER_FOREGROUND_FORBIDDEN', 409);
    if (operation.engine === 'chrome-devtools') {
      if (background && ((descriptor.toolName === 'new_page' && !properties.background) ||
          (descriptor.toolName === 'select_page' && !properties.bringToFront)))
        if (allowForeground) background = false;
        else throw toolFailure('此连接未声明后台标签页参数，不能保证不激活浏览器。', 'BROWSER_BACKGROUND_UNSUPPORTED', 409);
      if (descriptor.toolName === 'new_page' && properties.background) prepared.background ??= background;
      if (descriptor.toolName === 'select_page' && properties.bringToFront) prepared.bringToFront ??= !background;
      if (properties.pageId && prepared.pageId == null && session.selectedTabId != null) prepared.pageId = Number(session.selectedTabId);
    }
    if (background && operation.engine === 'playwright' && !operation.headless &&
        descriptor.toolName === 'browser_tabs' && ['select', 'new'].includes(prepared.action)) {
      if (allowForeground) background = false;
      else throw toolFailure('此 Playwright 标签页操作没有后台参数。请使用支持后台标签页的连接。', 'BROWSER_BACKGROUND_UNSUPPORTED', 409);
    }
    const selecting = descriptor.toolName === 'select_page' || descriptor.toolName === 'browser_tabs';
    const tabId = prepared.pageId != null ? String(prepared.pageId) : selecting && prepared.index != null
      ? String(prepared.index) : session.selectedTabId ?? connection.selectedTabId;
    // Implicit-page APIs cannot safely act on another session's newly selected tab.
    // 隐式页面接口不能安全操作另一会话新选中的标签页。
    if (!selecting && !properties.pageId && session.selectedTabId != null && connection.selectedTabId != null &&
        session.selectedTabId !== connection.selectedTabId)
      throw toolFailure('浏览器标签页已由另一任务切换，请先重新选择并读取当前页。', 'BROWSER_TAB_CHANGED', 409);
    const refs = references(prepared), tab = connection.tabs.get(tabId);
    if (refs.length && tabId == null)
      throw toolFailure('当前快照尚无可验证的标签页 ID，请先列出标签页，再读取该页的新快照。', 'BROWSER_TAB_ID_REQUIRED', 409);
    if (refs.length && (!tab?.snapshot || tab.snapshot.sessionId !== session.sessionId || refs.some(ref => !tab.snapshot.refs.has(ref))))
      throw toolFailure('页面元素引用已过期或不属于当前聊天；请读取此标签页的新快照。', 'BROWSER_STALE_REFERENCE', 409);
    if (!operation.readOnly && session.needsObservation)
      throw toolFailure('上次浏览器操作结果尚未确认，请先读取当前页面的新快照。', 'BROWSER_OBSERVATION_REQUIRED', 409);
    const innerTimeout = operation.navigation ? 60000 : 15000;
    if (properties.timeout && (!Number.isInteger(prepared.timeout) || prepared.timeout === 0)) prepared.timeout = innerTimeout;
    const configuredTimeout = Number.isInteger(prepared.timeout) && prepared.timeout > 0 ? prepared.timeout : innerTimeout;
    if (configuredTimeout > 120000)
      throw toolFailure('浏览器动作期限不能超过 120 秒；长任务应拆成可验证步骤。', 'BROWSER_TIMEOUT_OUT_OF_RANGE', 409);
    const timeoutMs = operation.navigation ? Math.max(70000, configuredTimeout + 10000) : Math.max(30000, configuredTimeout + 10000);
    return { args: prepared, browser: { connectionId: descriptor.key, serverId: descriptor.serverId,
      engine: operation.engine, mode: operation.mode, sessionId: session.sessionId, toolName: descriptor.toolName,
      ...(tabId != null ? { tabId } : {}), ...(tab?.url ? { url: tab.url } : {}),
      ...(publicBrowserUrl(prepared.url) ? { requestedUrl: publicBrowserUrl(prepared.url) } : {}),
      readOnly: operation.readOnly, background, timeoutMs }, connection, session, operation };
  }

  /**
   * Capture only observed identity, without selecting a tab or inventing one.
   * 只捕获已观察到的身份，不主动切换标签页，也不虚构页面。
   */
  approvalIdentity(prepared) {
    if (!prepared.browser) return undefined;
    const { browser, args, connection } = prepared;
    const targetsPage = !['new_page', 'list_pages', 'browser_tabs'].includes(browser.toolName);
    return Object.freeze({ connectionId: browser.connectionId, sessionId: browser.sessionId,
      toolName: browser.toolName, targetsPage, tabId: browser.tabId,
      ...(references(args).length ? { snapshotGeneration: connection.tabs.get(browser.tabId)?.snapshot?.generation } : {}) });
  }

  /** Recovery can observe the same task's page; it cannot switch to another task's selected tab.
   * 恢复可以读取当前任务页面，但不能切换到其他任务选中的标签页。
   */
  observationRecovery(descriptor, args, server, options, code) {
    if (!['BROWSER_TAB_ID_REQUIRED', 'BROWSER_STALE_REFERENCE'].includes(code)) return undefined;
    const operation = browserOperation(descriptor, server);
    if (!operation) return undefined;
    const { connection, session } = this._state(descriptor, operation, options.sessionId);
    const requestedTabId = args.pageId != null ? String(args.pageId) : session.selectedTabId;
    if (requestedTabId != null && session.selectedTabId !== requestedTabId) return undefined;
    if (!descriptor.originalInputSchema?.properties?.pageId && session.selectedTabId != null &&
        connection.selectedTabId != null && session.selectedTabId !== connection.selectedTabId) return undefined;
    if (requestedTabId == null && !session.lastReceipt?.url) return undefined;
    return { sessionId: session.sessionId, engine: operation.engine, tabId: requestedTabId,
      connectionSelectedTabId: connection.selectedTabId,
      url: connection.tabs.get(requestedTabId)?.url ?? session.lastReceipt?.url };
  }

  verifyApprovalIdentity(expected, prepared) {
    if (!expected) return;
    const current = this.approvalIdentity(prepared);
    if (!current || current.connectionId !== expected.connectionId || current.sessionId !== expected.sessionId ||
        current.toolName !== expected.toolName)
      throw toolFailure('浏览器审批身份已变化，请重新准备调用和审批。', 'BROWSER_APPROVAL_CHANGED', 409);
    if (expected.targetsPage && current.tabId !== expected.tabId)
      throw toolFailure('审批期间浏览器目标标签页已变化，请读取目标页并重新审批。', 'BROWSER_TAB_CHANGED', 409);
    if (expected.snapshotGeneration != null && current.snapshotGeneration !== expected.snapshotGeneration)
      throw toolFailure('审批期间页面快照已变化，请读取新快照并重新审批。', 'BROWSER_STALE_REFERENCE', 409);
  }

  dispatch(prepared) {
    if (!prepared.browser || prepared.operation.readOnly) return;
    const tab = prepared.connection.tabs.get(prepared.browser.tabId);
    if (tab) delete tab.snapshot;
  }

  observe(prepared, result, outcome) {
    if (!prepared.browser) return undefined;
    const { connection, session, operation } = prepared, evidence = resultEvidence(result);
    const receipt = { ...prepared.browser, outcome, observedAt: new Date().toISOString() };
    if (outcome === 'unknown') { session.needsObservation = true; connection.generation++; }
    if (outcome === 'completed') {
      for (const item of evidence.tabs) {
        if (!connection.tabs.has(item.tabId) && connection.tabs.size >= maximumTabs) connection.tabs.delete(connection.tabs.keys().next().value);
        const tab = connection.tabs.get(item.tabId) ?? {};
        connection.tabs.set(item.tabId, { ...tab, url: item.url });
        if (item.selected) connection.selectedTabId = item.tabId;
      }
      const explicitTab = prepared.args.pageId != null ? String(prepared.args.pageId)
        : descriptorTabSelection(prepared) ?? connection.selectedTabId;
      if (explicitTab != null) {
        session.selectedTabId = explicitTab;
        const tab = connection.tabs.get(explicitTab) ?? {};
        if (evidence.pageUrl) tab.url = evidence.pageUrl;
        if (evidence.refs.size || snapshotTools.has(prepared.browser.toolName)) {
          tab.snapshot = { refs: evidence.refs, sessionId: session.sessionId, generation: ++connection.generation };
          session.needsObservation = false;
        }
        connection.tabs.set(explicitTab, tab); receipt.tabId = explicitTab;
        if (tab.url) receipt.url = tab.url;
        if (tab.snapshot) receipt.snapshotGeneration = tab.snapshot.generation;
        receipt.targetVerified = true;
      } else if (evidence.pageUrl) {
        // A first Playwright snapshot may have no tab listing. Preserve its
        // readable evidence but do not invent a stable tab ID or reusable refs.
        // 首次 Playwright 快照可能没有标签页清单；保留可读证据，但不编造稳定页面 ID 或可复用引用。
        receipt.url = evidence.pageUrl; receipt.targetVerified = false; receipt.needsTabDiscovery = true;
      }
      if (prepared.browser.toolName === 'close_page' || (prepared.browser.toolName === 'browser_tabs' && prepared.args.action === 'close')) {
        connection.tabs.delete(prepared.browser.tabId);
        if (connection.selectedTabId === prepared.browser.tabId) connection.selectedTabId = undefined;
        for (const known of this.sessions.values())
          if (known.connectionId === connection.connectionId && known.selectedTabId === prepared.browser.tabId) known.selectedTabId = undefined;
      }
    }
    session.lastReceipt = receipt; return receipt;
  }

  invalidate(connectionId) {
    const connection = this.connections.get(connectionId);
    if (connection) { connection.generation++; for (const tab of connection.tabs.values()) delete tab.snapshot; }
  }

  remove(connectionId) {
    this.connections.delete(connectionId);
    for (const [key, session] of this.sessions) if (session.connectionId === connectionId) this.sessions.delete(key);
  }

  diagnostics(sessionId) {
    return [...this.sessions.values()].filter(session => !sessionId || session.sessionId === sessionId)
      .map(session => session.lastReceipt ? structuredClone(session.lastReceipt) : null).filter(Boolean);
  }
}

function descriptorTabSelection(prepared) {
  if (prepared.browser.toolName === 'browser_tabs' && prepared.args.action === 'select' && prepared.args.index != null)
    return String(prepared.args.index);
  return undefined;
}
