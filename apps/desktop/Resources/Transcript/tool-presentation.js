/* Display projection only: execution identity, approval and full receipts stay in the host. */
(() => {
  'use strict';
  const titles = {
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
  const searches = new Set(['web_search_exa', 'web_search_exa_deep', 'brave_web_search', 'web_search', 'search_web']);
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
    const args = tool.arguments;
    if (!args || typeof args !== 'object' || Array.isArray(args)) return {};
    if (tool.name?.startsWith('mcp.') && args.policy && args.arguments && typeof args.arguments === 'object') return args.arguments;
    return args;
  }
  function action(tool, args) {
    if (tool.name === 'terminal.host.run') return plain(args.script, 2048);
    const commandArgs = Array.isArray(args.args) ? args.args.filter(value => typeof value === 'string') : [];
    const quote = value => /[\s"]/u.test(value) ? JSON.stringify(value) : value;
    if (typeof args.command === 'string') return [args.command, ...commandArgs.map(quote)].join(' ');
    if (tool.name === 'skill.run' && typeof args.path === 'string') return ['node', quote(args.path), ...commandArgs.map(quote)].join(' ');
    for (const field of ['path', 'filePath', 'query', 'search_query', 'pattern', 'action', 'method']) {
      const value = plain(args[field], 2048);
      if (value) return value;
    }
    for (const field of ['url', 'uri']) {
      if (typeof args[field] !== 'string') continue;
      try {
        const url = new URL(args[field]);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) continue;
        if ([...url.searchParams.keys()].some(key => /key|token|password|auth|secret/i.test(key))) continue;
        return args[field];
      } catch { /* An invalid address is not a display action. */ }
    }
    return '';
  }
  function computerActions(tool, args) {
    const parts = [];
    const number = key => Number.isSafeInteger(args[key]) ? args[key] : null;
    const point = (x, y) => number(x) !== null && number(y) !== null ? `(${number(x)}, ${number(y)})` : '';
    if (typeof args.windowId === 'string' && /^[0-9]{1,20}$/u.test(args.windowId) && number('processId') > 0)
      parts.push(`HWND ${args.windowId} · PID ${number('processId')}`);
    if (tool.name === 'computer.launch') {
      const app = plain(args.appPath, 2048), argumentsList = Array.isArray(args.args) ? args.args.filter(value => typeof value === 'string') : [];
      const quote = value => !value || /[\s"]/u.test(value) ? '"' + value.replace(/"/g, '\\"') + '"' : value;
      if (app) parts.push([quote(app), ...argumentsList.map(quote)].join(' '));
      if (args.background === true) parts.push({ key: 'toolBackgroundLaunch', values: [] });
    }
    if (['computer.click', 'computer.move', 'computer.scroll'].includes(tool.name)) parts.push(point('x', 'y'));
    if (tool.name === 'computer.drag') {
      const start = point('x', 'y'), end = point('endX', 'endY');
      if (start && end) parts.push(start + ' → ' + end);
    }
    if (tool.name === 'computer.scroll' && number('delta') !== null) parts.push({ key: 'toolScrollDelta', values: [number('delta')] });
    if (tool.name === 'computer.type') parts.push({ key: 'toolCharacterCount', values: [typeof args.text === 'string' ? args.text.length : 0] });
    if (tool.name === 'computer.key') parts.push(plain(args.key, 120));
    if (tool.name === 'computer.window') {
      const modes = { resize: 'toolWindowResize', maximize: 'toolWindowMaximize', minimize: 'toolWindowMinimize', restore: 'toolWindowRestore' };
      if (Object.hasOwn(modes, args.mode)) parts.push({ key: modes[args.mode], values: [] });
      if (args.mode === 'resize' && number('width') > 0 && number('height') > 0) parts.push(`${number('width')} × ${number('height')} px`);
    }
    if (tool.name === 'computer.screenshot' && args.crop && typeof args.crop === 'object') {
      const { x, y, width, height } = args.crop;
      if ([x, y, width, height].every(Number.isSafeInteger) && x >= 0 && y >= 0 && width > 0 && height > 0)
        parts.push(`(${x}, ${y}) · ${width} × ${height} px`);
    }
    return parts.filter(Boolean);
  }
  function readableError(value, depth = 0) {
    if (depth > 3 || value == null) return '';
    if (typeof value === 'string') {
      if (value.length > 65536) return '';
      try { return readableError(JSON.parse(value), depth + 1); } catch { return plain(value); }
    }
    if (typeof value !== 'object' || Array.isArray(value)) return '';
    // Read only documented public error fields. Never scan metadata, parameters or arbitrary result objects.
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
    const leaf = String(tool.name || '').split('.').at(-1);
    const titleKey = website ? searches.has(leaf) ? 'toolSearchWeb' : 'toolReadWeb' : titles[tool.name] || 'toolExecute';
    const args = businessArguments(tool);
    const computer = tool.name?.startsWith('computer.') && Object.hasOwn(titles, tool.name);
    const summary = tool.summary === tool.name ? '' : plain(tool.summary);
    const intent = plain(tool.arguments?.policy?.reason) || plain(args.reason) || summary;
    const failed = ['error', 'denied', 'unknown', 'cancelled'].includes(tool.status);
    const errorKey = failed ? errorKeys[tool.code] || (tool.status === 'unknown' ? 'toolOutcomeUncertain' : '') : '';
    return { website, titleKey, action: website || computer ? '' : action(tool, args), intent: website || computer ? '' : intent,
      actionParts: computer ? computerActions(tool, args) : null,
      errorKey, errorText: failed && !errorKey ? readableError(tool.result) : '' };
  }
  window.KynxaToolPresentation = Object.freeze({ describe });
})();
