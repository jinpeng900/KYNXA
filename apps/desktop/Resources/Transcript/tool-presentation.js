/* Display projection only: execution identity, approval and full receipts stay in the host.
 * 仅生成展示投影；执行身份、审批与完整回执仍由宿主保存。 */
(() => {
  'use strict';
  const toolTitleKeys = {
    'filesystem.read': 'toolReadFile', 'file.read': 'toolReadFile', 'filesystem.stat': 'toolInspectFile',
    'filesystem.list': 'toolListFiles', 'filesystem.search': 'toolSearchFiles',
    'filesystem.write': 'toolEditFile', 'filesystem.edit': 'toolEditFile',
    'filesystem.delete': 'toolDeleteFile', 'filesystem.mkdir': 'toolCreateFolder',
    'terminal.run': 'toolRunCommand', 'terminal.host.run': 'toolRunCommand', 'skill.run': 'toolUseSkill', 'skill.read': 'toolReadSkill',
    'skill.resource.read': 'toolReadSkill', 'skill.list': 'toolFindSkill',
    'skill.inspect': 'toolInspectSkill', 'skill.check': 'toolInspectSkill',
    'tool.search': 'toolFindTools', 'tool.load': 'toolFindTools', 'tool.result.read': 'toolReadResult',
    'conversation.history.search': 'toolFindHistory', 'conversation.history.read': 'toolReadHistory',
    'computer.windows': 'toolInspectWindows', 'computer.apps': 'toolFindApps', 'computer.screenshot': 'toolScreenshot', 'computer.read': 'toolReadWindow',
    'computer.launch': 'toolOpenApp', 'computer.window': 'toolAdjustWindow', 'computer.activate': 'toolActivateWindow', 'computer.click': 'toolClick',
    'computer.move': 'toolMovePointer', 'computer.scroll': 'toolScroll', 'computer.drag': 'toolDrag',
    'computer.type': 'toolTypeText', 'computer.key': 'toolPressKey'
  };
  const searchToolNames = new Set(['web_search_exa', 'web_search_exa_deep', 'brave_web_search', 'web_search', 'search_web']);
  const errorKeys = {
    MCP_TIMEOUT: 'toolTimedOut', WEB_TIMEOUT: 'toolTimedOut', TOOL_TIMEOUT: 'toolTimedOut', TOOL_TIMED_OUT: 'toolTimedOut', SANDBOX_HOST_TIMEOUT: 'toolTimedOut',
    DESKTOP_TIMEOUT: 'toolTimedOut', DESKTOP_TIMED_OUT: 'toolTimedOut', DESKTOP_READ_TIMEOUT: 'toolTimedOut',
    DESKTOP_NOT_RESPONDING: 'toolWindowUnresponsive',
    SANDBOX_UNAVAILABLE: 'toolSandboxUnavailable', SANDBOX_EXECUTION_UNAVAILABLE: 'toolSandboxUnavailable',
    APP_SKILL_ENVIRONMENT_UNAVAILABLE: 'toolSkillUnavailable',
    SANDBOX_COMMAND_UNSUPPORTED: 'toolCommandUnavailable', SANDBOX_SKILL_UNSUPPORTED: 'toolSkillUnavailable',
    MCP_NOT_CONNECTED: 'toolConnectionUnavailable', MCP_CONNECTION_FAILED: 'toolConnectionUnavailable', MCP_CONNECTION_LOST: 'toolConnectionUnavailable',
    MCP_AUTH_REQUIRED: 'toolAuthRequired',
    APPROVAL_EXPIRED: 'toolApprovalExpired', TOOL_APPROVAL_EXPIRED: 'toolApprovalExpired'
  };
  function plain(value, limit = 280) {
    if (typeof value !== 'string') return '';
    const text = value.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/g, '').trim();
    if (!text) return '';
    if (/^[\[{]/u.test(text)) {
      try { if (typeof JSON.parse(text) === 'object') return ''; } catch { return ''; }
    }
    return text.length > limit ? text.slice(0, limit) + '…' : text;
  }
  function businessArguments(tool) {
    const businessParameters = tool.arguments;
    if (!businessParameters || typeof businessParameters !== 'object' || Array.isArray(businessParameters)) return {};
    if (tool.name?.startsWith('mcp.') && businessParameters.policy && businessParameters.arguments && typeof businessParameters.arguments === 'object') return businessParameters.arguments;
    return businessParameters;
  }
  function action(tool, businessParameters) {
    if (tool.name === 'terminal.host.run') return plain(businessParameters.script, 2048);
    const commandArgs = Array.isArray(businessParameters.args) ? businessParameters.args.filter(value => typeof value === 'string') : [];
    const quoteCommandArgument = value => /[\s"]/u.test(value) ? JSON.stringify(value) : value;
    if (typeof businessParameters.command === 'string') return [businessParameters.command, ...commandArgs.map(quoteCommandArgument)].join(' ');
    if (tool.name === 'skill.run' && typeof businessParameters.path === 'string') return ['node', quoteCommandArgument(businessParameters.path), ...commandArgs.map(quoteCommandArgument)].join(' ');
    for (const field of ['path', 'filePath', 'query', 'search_query', 'pattern', 'action', 'method']) {
      const value = plain(businessParameters[field], 2048);
      if (value) return value;
    }
    for (const field of ['url', 'uri']) {
      if (typeof businessParameters[field] !== 'string') continue;
      try {
        const url = new URL(businessParameters[field]);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) continue;
        if ([...url.searchParams.keys()].some(key => /key|token|password|auth|secret/i.test(key))) continue;
        return businessParameters[field];
      } catch { /* An invalid address is not a display action. 无效地址不能作为可展示的操作。 */ }
    }
    return '';
  }
  function computerActions(tool, businessParameters) {
    const operationParts = [];
    const integerArgument = key => Number.isSafeInteger(businessParameters[key]) ? businessParameters[key] : null;
    const pointText = (x, y) => integerArgument(x) !== null && integerArgument(y) !== null ? `(${integerArgument(x)}, ${integerArgument(y)})` : '';
    if (typeof businessParameters.windowId === 'string' && /^[0-9]{1,20}$/u.test(businessParameters.windowId) && integerArgument('processId') > 0)
      operationParts.push(`HWND ${businessParameters.windowId} · PID ${integerArgument('processId')}`);
    if (tool.name === 'computer.launch') {
      const app = plain(businessParameters.appPath, 2048), argumentsList = Array.isArray(businessParameters.args) ? businessParameters.args.filter(value => typeof value === 'string') : [];
      const quoteCommandArgument = value => !value || /[\s"]/u.test(value) ? '"' + value.replace(/"/g, '\\"') + '"' : value;
      if (app) operationParts.push([quoteCommandArgument(app), ...argumentsList.map(quoteCommandArgument)].join(' '));
      if (businessParameters.background === true) operationParts.push({ key: 'toolBackgroundLaunch', values: [] });
    }
    if (['computer.click', 'computer.move', 'computer.scroll'].includes(tool.name)) operationParts.push(pointText('x', 'y'));
    if (tool.name === 'computer.drag') {
      const start = pointText('x', 'y'), end = pointText('endX', 'endY');
      if (start && end) operationParts.push(start + ' → ' + end);
    }
    if (tool.name === 'computer.scroll' && integerArgument('delta') !== null) operationParts.push({ key: 'toolScrollDelta', values: [integerArgument('delta')] });
    if (tool.name === 'computer.type') operationParts.push({ key: 'toolCharacterCount', values: [typeof businessParameters.text === 'string' ? businessParameters.text.length : 0] });
    if (tool.name === 'computer.key') operationParts.push(plain(businessParameters.key, 120));
    if (tool.name === 'computer.window') {
      const modes = { resize: 'toolWindowResize', maximize: 'toolWindowMaximize', minimize: 'toolWindowMinimize', restore: 'toolWindowRestore' };
      if (Object.hasOwn(modes, businessParameters.mode)) operationParts.push({ key: modes[businessParameters.mode], values: [] });
      if (businessParameters.mode === 'resize' && integerArgument('width') > 0 && integerArgument('height') > 0) operationParts.push(`${integerArgument('width')} × ${integerArgument('height')} px`);
    }
    if (tool.name === 'computer.screenshot' && businessParameters.crop && typeof businessParameters.crop === 'object') {
      const { x, y, width, height } = businessParameters.crop;
      if ([x, y, width, height].every(Number.isSafeInteger) && x >= 0 && y >= 0 && width > 0 && height > 0)
        operationParts.push(`(${x}, ${y}) · ${width} × ${height} px`);
    }
    return operationParts.filter(Boolean);
  }
  function readableError(value, depth = 0) {
    if (depth > 3 || value == null) return '';
    if (typeof value === 'string') {
      if (value.length > 65536) return '';
      try { return readableError(JSON.parse(value), depth + 1); } catch { return plain(value); }
    }
    if (typeof value !== 'object' || Array.isArray(value)) return '';
    // Read only documented public error fields. Never scan metadata, parameters or arbitrary result objects.
    // 只读取已有约定的公开错误字段；不扫描元数据、参数或任意结果对象。
    const message = plain(value.message) || plain(value.error?.message);
    if (message) return message;
    if (value.truncated === true && typeof value.totalCharacters === 'number') return readableError(value.preview, depth + 1);
    if (Array.isArray(value.content)) {
      for (const block of value.content) {
        if (block?.type === 'text') {
          const text = readableError(block.text, depth + 1);
          if (text) return text;
        }
      }
    }
    return '';
  }
  function describe(tool) {
    const website = window.KynxaToolWebLinks.extract(tool) !== null;
    const toolName = String(tool.name || '').split('.').at(-1);
    const titleKey = website ? searchToolNames.has(toolName) ? 'toolSearchWeb' : 'toolReadWeb' : toolTitleKeys[tool.name] || 'toolExecute';
    const businessParameters = businessArguments(tool);
    const computer = tool.name?.startsWith('computer.') && Object.hasOwn(toolTitleKeys, tool.name);
    const summary = tool.summary === tool.name ? '' : plain(tool.summary);
    const intent = plain(tool.arguments?.policy?.reason) || plain(businessParameters.reason) || summary;
    const failed = ['error', 'denied', 'unknown', 'cancelled'].includes(tool.status);
    const errorKey = failed ? errorKeys[tool.code] || (tool.status === 'unknown' ? 'toolOutcomeUncertain' : '') : '';
    return { website, titleKey, action: website || computer ? '' : action(tool, businessParameters), intent: website || computer ? '' : intent,
      actionParts: computer ? computerActions(tool, businessParameters) : null,
      errorKey, errorText: failed && !errorKey ? readableError(tool.result) : '' };
  }
  window.KynxaToolPresentation = Object.freeze({ describe });
})();
