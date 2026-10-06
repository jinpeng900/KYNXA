/* One browser selection surface for the whole conversation. KaTeX stays real DOM.
 * 整个聊天共用浏览器选择表面；KaTeX 保持真实 DOM。 */
(() => {
  'use strict';
  const messages = document.getElementById('messages');
  const jumpButton = document.getElementById('jump-to-latest');
  const COPY_ICON = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
  const COPY_SUCCESS_ICON = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><path d="m5 12 4 4L19 6"/></svg>';
  const uiStrings = {
    conversation: '对话', transcript: '聊天记录', copy: '复制', copyMessage: '复制整条消息', retry: '重试',
    copied: '已复制', copyFailed: '复制失败，请重试。', jumpToLatest: '跳转到最新消息',
    reasoning: '思考过程', thinking: '正在思考…', reasoningDuration: '思考过程 · {0} 秒', stopped: '已停止生成',
    interrupted: '回复中断，请重试。', replying: '正在回复…', generating: '正在生成',
    toolActivities: '工具活动', toolRunning: '执行中', toolCompleted: '已完成', toolError: '工具失败',
    toolDenied: '已拒绝', toolApproval: '等待批准', toolCancelled: '已取消', toolUnknown: '结果未知',
    toolSearchWeb: '搜索网页', toolReadWeb: '阅读网页', toolReadFile: '读取文件', toolInspectFile: '查看文件',
    toolListFiles: '查看文件夹', toolSearchFiles: '查找文件', toolEditFile: '修改文件', toolDeleteFile: '删除文件',
    toolCreateFolder: '创建文件夹', toolRunCommand: '运行命令', toolUseSkill: '使用技能', toolReadSkill: '读取技能',
    toolFindSkill: '查找技能', toolInspectSkill: '检查技能', toolFindTools: '查找工具', toolReadResult: '读取工具记录',
    toolFindHistory: '查找聊天记录', toolReadHistory: '读取聊天记录', toolExecute: '执行操作',
    toolFindSources: '查找资料', toolReadSource: '读取资料',
    toolOutcomeUncertain: '操作已中断，执行结果尚未确定。', toolTimedOut: '操作超时。',
    toolSandboxUnavailable: '沙箱暂不可用。', toolSkillUnavailable: '技能运行环境尚未满足。', toolApprovalExpired: '批准已过期。',
    toolCommandUnavailable: '此命令暂不支持。', toolConnectionUnavailable: '工具连接不可用。', toolAuthRequired: '工具需要认证。',
    toolMoreWebLinks: '另{0}个链接',
    toolInspectWindows: '查看窗口', toolFindApps: '查找软件', toolScreenshot: '截图', toolReadWindow: '读取窗口', toolOpenApp: '打开软件',
    toolActivateWindow: '切换窗口', toolClick: '点击', toolMovePointer: '移动鼠标', toolScroll: '滚动', toolDrag: '拖动',
    toolTypeText: '输入文字', toolPressKey: '按下按键', toolCharacterCount: '{0}字符', toolScrollDelta: '滚动量 {0}',
    toolAdjustWindow: '调整窗口', toolWindowResize: '调整大小', toolWindowMaximize: '最大化', toolWindowMinimize: '最小化',
    toolWindowRestore: '恢复窗口', toolBackgroundLaunch: '后台启动', toolWindowUnresponsive: '窗口未响应',
    toolViewScreenshot: '查看截图',
    messageSentAt: '发送时间', replyCreatedAt: '回复创建时间', replyEndedAt: '回复结束时间',
    localEndTime: '本机记录', timeNotRecorded: '未记录',
    elapsedSeconds: '用时 {0}秒', elapsedMinutesSeconds: '用时 {0}分钟{1}秒', elapsedHoursMinutesSeconds: '用时 {0}小时{1}分钟{2}秒'
  };
  let entries = new Map();
  const conversations = new Map();
  let cachedCharacters = 0, cachedNodes = 0;
  const maxCachedCharacters = 2 * 1024 * 1024, maxCachedNodes = 40000;
  const mathCache = new Map();
  let conversationId = null, pending = null, following = true, applying = false;
  let nextMathId = 0, mathCacheBytes = 0, scrollFrame = 0, flushFrame = 0;
  let pointerSelecting = false, localizingUi = false, languageFrame = 0;
  let anchoringScroll = false, anchorFrame = 0;
  let bottomNavigationPending = false, navigationGeneration = 0;
  let nextCopyRequest = 0, copyFeedbackTimer = 0, copyFeedbackEntry = null;
  const pendingCopies = new Map();
  const messageTimeFormatters = new Map();
  const liveElapsedEntries = new Set();
  let elapsedTimer = null;
  const send = value => window.chrome?.webview?.postMessage(value);
  const presentationFor = message => window.KynxaMessagePresentation.selectMessagePresentation(message);
  function refreshElapsedText(entry) {
    const durationMs = entry.elapsedMode === 'live'
      ? Math.floor(Math.max(0, performance.now() - entry.generationStartedAtMs)) : entry.finalDurationMs;
    const text = entry.elapsedMode ? window.KynxaMessagePresentation.elapsedText(durationMs, uiStrings,
      { live: entry.elapsedMode === 'live' }) : '';
    setToolText(entry.elapsed, text);
    // Reused snapshots must not mutate unchanged attributes or disturb the cached selection surface.
    // 复用快照时不重复写入未变属性，保留缓存聊天的选区与展示节点。
    if (entry.elapsed.hidden !== !text) entry.elapsed.hidden = !text;
    const mode = entry.elapsedMode || '';
    if (entry.elapsed.dataset.mode !== mode) entry.elapsed.dataset.mode = mode;
  }
  function synchronizeElapsedTimer() {
    const hasVisibleClock = !document.hidden && [...liveElapsedEntries].some(entry => entry.article.isConnected);
    if (hasVisibleClock && elapsedTimer === null) elapsedTimer = setInterval(() => {
      for (const entry of liveElapsedEntries) if (entry.article.isConnected) refreshElapsedText(entry);
      synchronizeElapsedTimer();
    }, 1000);
    else if (!hasVisibleClock && elapsedTimer !== null) { clearInterval(elapsedTimer); elapsedTimer = null; }
  }
  function updateElapsedState(entry, message) {
    const presentation = presentationFor(message);
    if (presentation.mode === 'final') {
      entry.elapsedMode = 'final'; entry.finalDurationMs = message.durationMs;
      liveElapsedEntries.delete(entry);
    } else if (message.role === 'assistant' && message.streaming && Number.isSafeInteger(message.generationElapsedMs)
        && message.generationElapsedMs >= 0) {
      // Anchor each attempt once; tokens, rounds, language and cached navigation keep the ongoing clock.
      // 每次尝试仅定位一次；token、轮次、语言与缓存聊天切换保留本次计时，重试则使用新的起点。
      if (entry.elapsedMode !== 'live') entry.generationStartedAtMs = performance.now() - message.generationElapsedMs;
      entry.elapsedMode = 'live'; liveElapsedEntries.add(entry);
    } else {
      entry.elapsedMode = null; delete entry.generationStartedAtMs; liveElapsedEntries.delete(entry);
    }
    refreshElapsedText(entry);
  }
  function statusText(message, presentation = presentationFor(message)) {
    if (presentation.mode === 'incomplete') return uiStrings.interrupted;
    if (message.status === 'interrupted')
      return uiStrings.stopped + (typeof message.error === 'string' && message.error ? ' · ' + message.error : '');
    if (message.error) return typeof message.error === 'string' ? message.error : uiStrings.interrupted;
    if (presentation.mode === 'active' && message.reasoningState === 'thinking') return uiStrings.thinking;
    return message.waiting && !message.content ? uiStrings.replying : '';
  }
  function setStatusText(entry, text) {
    const node = entry.status.firstChild;
    if (node?.nodeType === Node.TEXT_NODE) {
      if (text) { if (node.data !== text) node.data = text; }
      else node.remove();
    } else if (text) entry.status.prepend(document.createTextNode(text));
  }
  function formatMessageTime(value) {
    if (!Number.isSafeInteger(value) || value <= 0) return uiStrings.timeNotRecorded;
    const date = new Date(value);
    if (!Number.isFinite(date.getTime())) return uiStrings.timeNotRecorded;
    const language = document.documentElement.lang === 'en' ? 'en-US' : 'zh-CN';
    let formatter = messageTimeFormatters.get(language);
    if (!formatter) {
      formatter = new Intl.DateTimeFormat(language, { calendar: 'gregory', numberingSystem: 'latn',
        year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' });
      messageTimeFormatters.set(language, formatter);
    }
    const parts = Object.fromEntries(formatter.formatToParts(date).map(part => [part.type, part.value]));
    return `${parts.year}/${parts.month}/${parts.day} ${parts.hour}:${parts.minute}:${parts.second}`;
  }
  function updateMessageMetadata(entry) {
    const message = entry.message;
    if (!message) return;
    const english = document.documentElement.lang === 'en', separator = english ? ': ' : '：';
    const user = message.role === 'user';
    const lines = [(user ? uiStrings.messageSentAt : uiStrings.replyCreatedAt) + separator + formatMessageTime(message.createdAtMs)];
    if (!user) {
      const streaming = message.streaming === true || message.status === 'streaming';
      const ended = streaming ? uiStrings.generating : formatMessageTime(message.endRecordedAtMs);
      const recorded = !streaming && ended !== uiStrings.timeNotRecorded;
      const qualifier = recorded ? (english ? ` (${uiStrings.localEndTime})` : `（${uiStrings.localEndTime}）`) : '';
      lines.push(uiStrings.replyEndedAt + qualifier + separator + ended);
      const elapsed = !streaming ? window.KynxaMessagePresentation.elapsedText(message.durationMs, uiStrings) : '';
      if (elapsed) lines.push(elapsed);
    }
    // Times describe the message without becoming body text, selection content, or a guessed end time.
    // 时间只描述消息，不进入正文与选区，也不根据耗时猜测结束时刻。
    const description = lines.join('\n');
    if (entry.article.title !== description) entry.article.title = description;
    if (entry.article.getAttribute('aria-description') !== description) entry.article.setAttribute('aria-description', description);
  }
  function localizeEntry(entry) {
    localizeCopyButton(entry);
    entry.retry.textContent = uiStrings.retry;
    if (entry.message) {
      updateMessageMetadata(entry);
      setStatusText(entry, statusText(entry.message));
      refreshElapsedText(entry);
    }
    entry.status.querySelector('.streaming-dot')?.setAttribute('aria-label', uiStrings.generating);
    for (const row of entry.toolRows?.values() || []) localizeToolRow(row);
    for (const row of entry.segmentRows?.values() || []) {
      for (const tool of row.toolRows.values()) localizeToolRow(tool);
    }
  }
  function localizeCopyButton(entry) {
    const state = entry.copy.dataset.copyState;
    entry.copy.title = state === 'success' ? uiStrings.copied : state === 'error' ? uiStrings.copyFailed : uiStrings.copy;
    entry.copy.setAttribute('aria-label', state === 'success' ? uiStrings.copied : state === 'error' ? uiStrings.copyFailed : uiStrings.copyMessage);
  }
  function clearCopyFeedback() {
    clearTimeout(copyFeedbackTimer);
    if (copyFeedbackEntry) {
      delete copyFeedbackEntry.copy.dataset.copyState;
      copyFeedbackEntry.copy.innerHTML = COPY_ICON;
      localizeCopyButton(copyFeedbackEntry);
    }
    copyFeedbackEntry = null;
  }
  function requestCopy(entry) {
    const requestId = String(++nextCopyRequest);
    entry.copyRequestId = requestId;
    while (pendingCopies.size >= 32) pendingCopies.delete(pendingCopies.keys().next().value);
    pendingCopies.set(requestId, { entry, conversationId });
    send({ type: 'copy', conversationId, requestId, messageId: entry.message.id, text: copiedMessage(entry.message) });
  }
  function acknowledgeCopy(command) {
    const request = pendingCopies.get(command.requestId);
    pendingCopies.delete(command.requestId);
    if (!request || command.conversationId !== conversationId || request.conversationId !== conversationId
      || request.entry.copyRequestId !== command.requestId || !request.entry.article.isConnected) return;
    // Show success only after the native host has written the clipboard, and ignore old operations.
    // 只有原生宿主写入剪贴板后才显示成功，忽略已经过期的操作回执。
    clearCopyFeedback();
    copyFeedbackEntry = request.entry;
    copyFeedbackEntry.copy.dataset.copyState = command.success === true ? 'success' : 'error';
    copyFeedbackEntry.copy.innerHTML = command.success === true ? COPY_SUCCESS_ICON : COPY_ICON;
    localizeCopyButton(copyFeedbackEntry);
    copyFeedbackTimer = setTimeout(clearCopyFeedback, 2200);
  }
  function initializeUi(command) {
    const left = scrollX, top = scrollY;
    // Startup has no content to preserve; let its first render follow the bottom.
    // 首次启动没有需要保留的内容，让第一次渲染跟随底部。
    localizingUi = entries.size > 0;
    if (localizingUi) cancelAnimationFrame(scrollFrame);
    cancelAnimationFrame(languageFrame);
    for (const key of Object.keys(uiStrings))
      if (typeof command.strings?.[key] === 'string') uiStrings[key] = command.strings[key];
    document.documentElement.lang = command.language === 'en' ? 'en' : 'zh-CN';
    document.title = uiStrings.conversation;
    messages.setAttribute('aria-label', uiStrings.transcript);
    jumpButton.textContent = '↓ ' + uiStrings.jumpToLatest;
    jumpButton.setAttribute('aria-label', uiStrings.jumpToLatest);
    jumpButton.title = uiStrings.jumpToLatest;
    // Localize only application controls; message DOM and browser selections stay intact.
    // 只本地化应用控件；消息 DOM 与浏览器选择保持不变。
    for (const entry of entries.values()) localizeEntry(entry);
    for (const saved of conversations.values()) for (const entry of saved.entries.values()) localizeEntry(entry);
    if (!localizingUi) return;
    // Labels may wrap differently. Keep the user's scroll position and suppress only
    // the automatic bottom-follow triggered by this layout, without touching ranges.
    // 文案换行可能改变布局；保留用户滚动位置，仅暂停本次布局触发的底部跟随，不修改选择范围。
    if (scrollX !== left || scrollY !== top) scrollTo({ left, top, behavior: 'instant' });
    languageFrame = requestAnimationFrame(() => {
      languageFrame = requestAnimationFrame(() => {
        localizingUi = false;
        if (bottomNavigationPending) followBottom();
      });
    });
  }
  const atBottom = () => document.documentElement.scrollHeight - innerHeight - scrollY <= 36;
  const updateJumpButton = () => { jumpButton.hidden = entries.size === 0 || atBottom(); };
  const activeSelection = () => {
    const selection = getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return false;
    try { return selection.getRangeAt(0).intersectsNode(messages); } catch { return false; }
  };
  function followBottom() {
    if (localizingUi || !following || activeSelection() || pointerSelecting) return;
    const expectedConversation = conversationId, expectedGeneration = navigationGeneration;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = requestAnimationFrame(() => {
      if (conversationId !== expectedConversation || navigationGeneration !== expectedGeneration || localizingUi) return;
      if (following && !activeSelection() && !pointerSelecting) scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' });
      bottomNavigationPending = false;
      updateJumpButton();
    });
  }
  function beginBottomNavigation() {
    following = true;
    bottomNavigationPending = true;
    navigationGeneration++;
  }
  function stopFollowing() {
    following = false;
    bottomNavigationPending = false;
    navigationGeneration++;
  }
  function jumpToLatest() {
    // Explicit navigation preserves the selection; selected text continues to freeze transcript updates.
    // 显式跳转保留选区；已选文字仍继续冻结聊天更新。
    following = !activeSelection();
    bottomNavigationPending = false;
    navigationGeneration++;
    cancelAnimationFrame(scrollFrame);
    scrollTo({ top: document.documentElement.scrollHeight, left: scrollX, behavior: 'instant' });
    updateJumpButton();
  }
  jumpButton.addEventListener('pointerdown', event => { if (event.button === 0) event.preventDefault(); });
  jumpButton.addEventListener('click', jumpToLatest);
  function clearSelection() {
    getSelection()?.removeAllRanges();
    pointerSelecting = false;
    scheduleFlush();
  }
  function scheduleFlush() {
    cancelAnimationFrame(flushFrame);
    flushFrame = requestAnimationFrame(() => {
      if (pending && !activeSelection()) { const snapshot = pending; pending = null; applyTranscript(snapshot); }
    });
  }
  function renderMath(root) {
    for (const math of root.querySelectorAll('.math[data-latex]')) {
      math.dataset.mathId = String(++nextMathId);
      const latex = math.dataset.latex || '', displayMode = math.dataset.display === 'true';
      if (latex.length > 32768) continue;
      const key = (displayMode ? 'D:' : 'I:') + latex;
      let html = mathCache.get(key);
      if (html === undefined) {
        const options = { displayMode, throwOnError: true, trust: false, maxExpand: 1000, maxSize: 100 };
        try { html = katex.renderToString(latex, options); }
        catch {
          try { html = katex.renderToString(latex, { ...options, strict: 'ignore', throwOnError: false }); }
          catch { math.textContent = latex; math.classList.add('katex-error'); continue; }
        }
        while (mathCache.size && (mathCache.size >= 512 || mathCacheBytes + html.length > 4 * 1024 * 1024)) {
          const oldest = mathCache.keys().next().value;
          mathCacheBytes -= mathCache.get(oldest).length;
          mathCache.delete(oldest);
        }
        if (html.length <= 4 * 1024 * 1024) { mathCache.set(key, html); mathCacheBytes += html.length; }
      }
      math.innerHTML = html;
    }
    for (const table of root.querySelectorAll('table')) {
      if (table.parentElement?.classList.contains('table-scroll')) continue;
      const scroll = document.createElement('div'); scroll.className = 'table-scroll';
      table.replaceWith(scroll); scroll.append(table);
    }
  }
  function parseBlocks(html) {
    const template = document.createElement('template'); template.innerHTML = html || '';
    template.content.querySelectorAll('script,iframe,object,embed,style,link,form').forEach(node => node.remove());
    for (const node of template.content.querySelectorAll('*'))
      for (const attribute of [...node.attributes])
        if (attribute.name.toLowerCase().startsWith('on')) node.removeAttribute(attribute.name);
    return [...template.content.childNodes].filter(node => node.nodeType !== Node.TEXT_NODE || node.textContent.trim());
  }
  function patchBlocks(container, html) {
    if (container._sourceHtml === html) return;
    const nodes = parseBlocks(html);
    const keys = nodes.map(node => node.nodeType === Node.ELEMENT_NODE ? node.outerHTML : node.textContent);
    const previous = container._blockKeys || [];
    let keep = 0;
    while (keep < keys.length && keep < previous.length && keys[keep] === previous[keep]) keep++;
    while (container.childNodes.length > keep) container.lastChild.remove();
    for (let i = keep; i < nodes.length; i++) {
      const fragment = document.createDocumentFragment(); fragment.append(nodes[i]);
      renderMath(fragment); container.append(fragment);
    }
    container._sourceHtml = html;
    container._blockKeys = keys;
  }
  function createMessage(message) {
    const article = document.createElement('article');
    article.className = 'message'; article.dataset.messageId = String(message.id);
    const content = document.createElement('div'); content.className = 'message-content';
    const elapsed = document.createElement('div'); elapsed.className = 'message-elapsed'; elapsed.dataset.copyIgnore = ''; elapsed.hidden = true;
    const body = document.createElement('div'); body.className = 'message-body';
    const status = document.createElement('div'); status.className = 'message-status'; status.dataset.copyIgnore = '';
    const actions = document.createElement('div'); actions.className = 'message-actions'; actions.dataset.copyIgnore = '';
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'copy-message';
    copy.title = uiStrings.copy; copy.setAttribute('aria-label', uiStrings.copyMessage);
    copy.innerHTML = COPY_ICON;
    const retry = document.createElement('button'); retry.type = 'button'; retry.className = 'retry'; retry.textContent = uiStrings.retry;
    const tools = document.createElement('div'); tools.className = 'tool-activities'; tools.hidden = true;
    const timeline = document.createElement('div'); timeline.className = 'assistant-timeline'; timeline.hidden = true;
    const entry = { article, content, elapsed, body, status, actions, copy, retry,
      tools, toolRows: new Map(), timeline, segmentRows: new Map(), message: null, presentation: null };
    copy.addEventListener('pointerdown', event => { if (event.button === 0) event.preventDefault(); });
    copy.addEventListener('click', () => requestCopy(entry));
    retry.addEventListener('click', () => send({ type: 'retry', conversationId, id: entry.message.id }));
    actions.append(copy, retry); content.append(elapsed, timeline, body, status); article.append(content, actions);
    return entry;
  }
  function updateMessage(entry, message) {
    const old = entry.message;
    updateElapsedState(entry, message);
    if (old && ['role', 'content', 'html', 'reasoningHtml', 'reasoningTitle', 'reasoningState', 'reasoningSeconds', 'status', 'waiting', 'streaming', 'error', 'canRetry', 'durationMs', 'generationElapsedMs', 'presentationMode', 'createdAtMs', 'endRecordedAtMs']
      .every(key => old[key] === message[key]) && !old.toolActivities?.length && !message.toolActivities?.length &&
      !old.assistantSegments?.length && !message.assistantSegments?.length) return;
    entry.message = message;
    updateMessageMetadata(entry);
    const presentation = presentationFor(message); entry.presentation = presentation;
    const role = message.role === 'user' ? 'user' : 'assistant';
    entry.article.className = 'message ' + role;
    entry.article.dataset.role = role;
    entry.article.dataset.presentationMode = presentation.mode;
    if (role === 'assistant' && !entry.avatar) {
      entry.avatar = document.createElement('img'); entry.avatar.className = 'message-avatar';
      entry.avatar.src = '../UI/Brand/kynxa-logo.png'; entry.avatar.alt = ''; entry.avatar.draggable = false; entry.avatar.dataset.copyIgnore = '';
      entry.article.prepend(entry.avatar);
    } else if (role === 'user' && entry.avatar) { entry.avatar.remove(); entry.avatar = null; }
    const segmented = role === 'assistant' && presentation.segments.length > 0;
    entry.timeline.hidden = !segmented; entry.body.hidden = segmented;
    updateAssistantSegments(entry, message, presentation);
    updateTools(entry, presentation.tools.filter(tool => !presentation.segments.some(segment => segment.round === tool.round)));
    if (!entry.tools.hidden && entry.tools.nextSibling !== entry.status) entry.content.insertBefore(entry.tools, entry.status);
    if (role === 'user') {
      const text = message.content || '';
      if (entry.body._plainContent !== text || !entry.body.hasAttribute('data-copy-plain')) {
        entry.body.textContent = text;
        entry.body.dataset.copyPlain = '';
        entry.body._plainContent = text;
        delete entry.body._sourceHtml; delete entry.body._blockKeys;
      }
    } else if (!segmented) {
      if (entry.body.hasAttribute('data-copy-plain')) {
        entry.body.removeAttribute('data-copy-plain'); entry.body.replaceChildren();
        delete entry.body._sourceHtml; delete entry.body._blockKeys;
      }
      patchBlocks(entry.body, message.html || '');
    }
    setStatusText(entry, statusText(message, presentation));
    const existingDot = entry.status.querySelector('.streaming-dot');
    if (message.streaming && !existingDot) { const dot = document.createElement('span'); dot.className = 'streaming-dot'; dot.setAttribute('aria-label', uiStrings.generating); entry.status.append(dot); }
    else if (!message.streaming) existingDot?.remove();
    entry.retry.hidden = !message.canRetry;
    entry.copy.hidden = !copiedMessage(message);
  }
  function copiedMessage(message) {
    return presentationFor(message).content;
  }
  function createSegmentRow(entry, id) {
    const root = document.createElement('section'); root.className = 'assistant-segment'; root.dataset.segmentId = id;
    const body = document.createElement('div'); body.className = 'message-body';
    const tools = document.createElement('div'); tools.className = 'tool-activities'; tools.hidden = true;
    root.append(body);
    return { root, body, tools, toolRows: new Map(), copy: entry.copy, message: entry.message, segment: null };
  }
  function updateAssistantSegments(entry, message, presentation) {
    const segments = presentation.segments;
    const ids = new Set(segments.map(segment => String(segment.id)));
    let previous = null;
    for (const segment of segments) {
      const id = String(segment.id);
      let row = entry.segmentRows.get(id);
      if (!row) { row = createSegmentRow(entry, id); entry.segmentRows.set(id, row); }
      const expected = previous ? previous.nextSibling : entry.timeline.firstChild;
      if (expected !== row.root) entry.timeline.insertBefore(row.root, expected);
      previous = row.root;
      row.message = message; row.segment = segment;
      const final = presentation.mode === 'final';
      const className = 'assistant-segment' + (final ? ' final-answer' : ' commentary');
      if (row.root.className !== className) row.root.className = className;
      if (row.root.dataset.phase !== segment.phase) row.root.dataset.phase = segment.phase;
      if (row.root.dataset.status !== segment.status) row.root.dataset.status = segment.status;
      if (row.root.dataset.round !== String(segment.round)) row.root.dataset.round = String(segment.round);
      patchBlocks(row.body, segment.html || '');
      const activities = presentation.tools.filter(tool => tool.round === segment.round)
        .sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
      updateTools(row, activities);
      if (!row.tools.hidden && row.tools.parentNode !== row.root) row.root.append(row.tools);
    }
    // Render the replacement body (including math) before removing earlier progress.
    // 先渲染替换后的正文与公式，再移除此前的中间进展。
    for (const [id, row] of entry.segmentRows) if (!ids.has(id)) { row.root.remove(); entry.segmentRows.delete(id); }
  }
  function localizeToolRow(row) {
    const labels = { running: uiStrings.toolRunning, completed: uiStrings.toolCompleted,
      error: uiStrings.toolError, denied: uiStrings.toolDenied, cancelled: uiStrings.toolCancelled,
      unknown: uiStrings.toolUnknown, 'approval-required': uiStrings.toolApproval };
    const title = uiStrings[row.presentation.titleKey] || uiStrings.toolExecute;
    setToolText(row.title, title + (row.count > 1 ? ' ×' + row.count : ''));
    setToolText(row.state, labels[row.tool.status] || uiStrings.toolUnknown);
    row.state.dataset.status = row.tool.status;
    const error = uiStrings[row.presentation.errorKey] || row.presentation.errorText;
    setToolText(row.error, error); row.error.hidden = !error;
    if (!row.website) {
      const action = [...new Set((row.actionPresentations || []).map(presentation => presentation.actionParts
        ? presentation.actionParts.map(part => typeof part === 'string' ? part : (part.values || []).reduce((text, value, index) =>
          text.replace('{' + index + '}', String(value)), uiStrings[part.key] || '')).filter(Boolean).join(' · ')
        : presentation.action).filter(Boolean))].join('\n');
      setToolText(row.command, action); row.command.hidden = !action; row.command.title = action;
    }
    if (row.more) {
      setToolText(row.more, row.moreCount > 0 ? uiStrings.toolMoreWebLinks.replace('{0}', String(row.moreCount)) : '');
      row.more.hidden = !row.moreCount;
    }
  }
  function setToolText(node, value) {
    if (node.textContent !== value) node.textContent = value;
  }
  function createToolRow(entry, id, website) {
    const root = document.createElement('div'); root.className = 'tool-activity'; root.dataset.toolCallId = id; root.dataset.copyIgnore = '';
    root.classList.toggle('tool-web-activity', website);
    const header = document.createElement('div'); header.className = 'tool-header';
    const title = document.createElement('span'); title.className = 'tool-title';
    const command = document.createElement('code'); command.className = 'tool-command';
    const state = document.createElement('span'); state.className = 'tool-state';
    const heading = document.createElement('span'); heading.className = 'tool-heading'; heading.append(title, state);
    header.append(heading, command);
    const error = document.createElement('div'); error.className = 'tool-error'; error.dataset.copyPlain = '';
    const links = document.createElement('div'); links.className = 'tool-web-links';
    const more = document.createElement('span'); more.className = 'tool-web-more'; more.hidden = true;
    root.append(header, ...(website ? [links, more] : []), error);
    return { root, title, state, command, error, links, more, moreCount: 0, website, tool: null, presentation: null, signature: null, count: 1 };
  }
  function toolGroups(entry, activities) {
    entry.toolSourceCache ??= new Map();
    const active = new Set(), groups = [];
    for (const tool of activities) {
      const id = String(tool.toolCallId); active.add(id);
      const signature = Number.isSafeInteger(tool.uiRevision) ? tool.uiRevision : JSON.stringify(tool);
      let view = entry.toolSourceCache.get(id);
      if (!view || view.signature !== signature) {
        const presentation = window.KynxaToolPresentation.describe(tool);
        view = { signature, tool, presentation, urls: presentation.website ? window.KynxaToolWebLinks.extract(tool) || [] : [] };
        entry.toolSourceCache.set(id, view);
      }
      const previous = groups.at(-1);
      const key = view.presentation.titleKey + ':' + view.presentation.website + ':' + tool.status;
      // Approval requests retain an individual, visible identity. Execution records are never merged.
      // 审批请求各自保留可见身份；正式执行记录不会被合并。
      if (previous && previous.key === key && ['completed', 'running'].includes(tool.status)) previous.views.push(view);
      else groups.push({ id, key, views: [view] });
    }
    for (const id of entry.toolSourceCache.keys()) if (!active.has(id)) entry.toolSourceCache.delete(id);
    return groups;
  }
  function updateTools(entry, activities) {
    entry.tools.hidden = activities.length === 0;
    const groups = toolGroups(entry, activities), ids = new Set(groups.map(group => group.id));
    for (const [id, row] of entry.toolRows) if (!ids.has(id)) { row.root.remove(); entry.toolRows.delete(id); }
    if (!activities.length) { entry.tools.remove(); return; }
    let previous = null;
    for (const group of groups) {
      const id = group.id, first = group.views[0];
      let row = entry.toolRows.get(id);
      if (!row) {
        row = createToolRow(entry, id, first.presentation.website); entry.toolRows.set(id, row);
      }
      const expected = previous ? previous.nextSibling : entry.tools.firstChild;
      if (expected !== row.root) entry.tools.insertBefore(row.root, expected);
      previous = row.root;
      // Immutable source records get a lightweight display revision from the desktop.
      // Historical/standalone clients fall back to a signature without changing formal events.
      // 不可变的源记录由桌面分配轻量展示版本；旧版或独立客户端回退到签名，正式事件不变。
      const signature = JSON.stringify(group.views.map(view => view.signature));
      if (row.signature === signature) continue;
      row.signature = signature;
      row.tool = first.tool; row.presentation = first.presentation; row.count = group.views.length;
      row.actionPresentations = group.views.map(view => view.presentation);
      row.root.dataset.toolCount = String(row.count);
      localizeToolRow(row);
      if (row.website) {
        const allUrls = [...new Set(group.views.flatMap(view => view.urls))], urls = allUrls.slice(0, 4);
        row.moreCount = allUrls.length - urls.length;
        setToolText(row.more, row.moreCount > 0 ? uiStrings.toolMoreWebLinks.replace('{0}', String(row.moreCount)) : '');
        row.more.hidden = !row.moreCount;
        const linksSignature = JSON.stringify(urls);
        if (row.linksSignature !== linksSignature) {
          row.linksSignature = linksSignature;
          row.links.replaceChildren(...urls.map(url => {
            const link = document.createElement('a'); link.className = 'tool-web-link';
            link.href = url; link.textContent = url; link.title = url; link.rel = 'noopener noreferrer';
            return link;
          }));
        }
        const action = urls.length ? '' : [...new Set(group.views.map(view => window.KynxaToolWebLinks.query(view.tool)).filter(Boolean))].join(' · ');
        setToolText(row.command, action); row.command.hidden = !action; row.command.title = action;
        continue;
      }
    }
  }
  function removeCached(id) {
    const saved = conversations.get(id);
    if (!saved) return null;
    conversations.delete(id); cachedCharacters -= saved.characters; cachedNodes -= saved.nodes;
    return saved;
  }
  function openConversation(id) {
    if (conversationId === id) return;
    pendingCopies.clear(); clearCopyFeedback();
    pending = null; clearSelection(); beginBottomNavigation();
    // Detach complete DOM trees: returning to a chat reuses KaTeX, code highlighting
    // and expanded reasoning instead of rebuilding every element.
    // 暂存完整 DOM 树；返回聊天时复用公式、高亮与展开状态，避免重建所有元素。
    const restored = removeCached(id);
    if (conversationId && entries.size) {
      let characters = 0;
      for (const entry of entries.values()) {
        for (const key of ['content', 'html', 'reasoningHtml']) characters += entry.message?.[key]?.length || 0;
        characters += JSON.stringify(entry.message?.toolActivities || []).length;
        characters += JSON.stringify(entry.message?.assistantSegments || []).length;
      }
      const nodes = messages.querySelectorAll('*').length;
      if (entries.size <= 400 && characters <= maxCachedCharacters && nodes <= maxCachedNodes) {
        const fragment = document.createDocumentFragment();
        while (messages.firstChild) fragment.append(messages.firstChild);
        removeCached(conversationId);
        conversations.set(conversationId, { entries, fragment, characters, nodes });
        cachedCharacters += characters; cachedNodes += nodes;
        while (conversations.size > 4 || cachedCharacters > maxCachedCharacters || cachedNodes > maxCachedNodes)
          removeCached(conversations.keys().next().value);
      }
    }
    entries = restored?.entries || new Map();
    messages.replaceChildren(...(restored ? [restored.fragment] : []));
    conversationId = id;
    updateJumpButton();
    liveElapsedEntries.clear();
    for (const entry of entries.values()) if (entry.elapsedMode === 'live') {
      liveElapsedEntries.add(entry); refreshElapsedText(entry);
    }
    synchronizeElapsedTimer();
    followBottom();
  }
  function applyTranscript(snapshot) {
    if (!snapshot || !Array.isArray(snapshot.messages)) return;
    const changedConversation = conversationId !== snapshot.conversationId;
    if (changedConversation) openConversation(snapshot.conversationId);
    if (changedConversation || snapshot.openAtBottom) {
      pending = null; clearSelection(); beginBottomNavigation();
    } else if (activeSelection()) {
      pending = snapshot;
      // Timing is outside selected prose. Terminal metadata stops the clock even while body convergence is deferred.
      // 计时位于选中正文之外；正文收束延迟时，结束元数据仍立即停止计时，保留选区与正文节点。
      const timingIds = new Set(snapshot.messages.map(message => String(message.id)));
      for (const entry of liveElapsedEntries) if (!timingIds.has(String(entry.message?.id))) liveElapsedEntries.delete(entry);
      for (const message of snapshot.messages) {
        const entry = entries.get(String(message.id)); if (entry) updateElapsedState(entry, message);
      }
      synchronizeElapsedTimer(); return;
    }
    const anchors = following ? [] : scrollAnchors();
    applying = true;
    const ids = new Set(snapshot.messages.map(message => String(message.id)));
    for (const [id, entry] of entries) if (!ids.has(id)) { liveElapsedEntries.delete(entry); entry.article.remove(); entries.delete(id); }
    let previous = null;
    for (const message of snapshot.messages) {
      const id = String(message.id);
      let entry = entries.get(id);
      if (!entry) { entry = createMessage(message); entries.set(id, entry); }
      updateMessage(entry, message);
      const expected = previous ? previous.nextSibling : messages.firstChild;
      if (expected !== entry.article) messages.insertBefore(entry.article, expected);
      previous = entry.article;
    }
    restoreScrollAnchor(anchors);
    applying = false;
    updateJumpButton();
    synchronizeElapsedTimer();
    followBottom();
    document.fonts.ready.then(followBottom);
  }
  function scrollAnchors() {
    const anchors = [], fallbacks = [];
    // Prefer a surviving paragraph in view; keep message-level fallbacks when transient steps disappear.
    // 优先使用视口内仍存在的段落作滚动锚点；中间阶段消失时仍保留消息级回退。
    for (const entry of entries.values()) {
      const bounds = entry.article.getBoundingClientRect();
      if (bounds.bottom <= 0 || bounds.top >= innerHeight) continue;
      for (const node of entry.article.querySelectorAll('.message-body > *, .tool-activity')) {
        const rect = node.getBoundingClientRect();
        if (rect.height && rect.bottom > 0 && rect.top < innerHeight) anchors.push({ node, top: rect.top });
        if (anchors.length >= 12) break;
      }
      fallbacks.push({ node: entry.article, top: bounds.top });
      if (anchors.length >= 12) break;
    }
    return [...anchors, ...fallbacks];
  }
  function restoreScrollAnchor(anchors) {
    for (const anchor of anchors) {
      if (!anchor.node.isConnected) continue;
      const bounds = anchor.node.getBoundingClientRect();
      if (!bounds.height) continue;
      const delta = bounds.top - anchor.top;
      if (Math.abs(delta) > .5) {
        anchoringScroll = true; cancelAnimationFrame(anchorFrame);
        scrollTo({ top: scrollY + delta, left: scrollX, behavior: 'instant' });
        anchorFrame = requestAnimationFrame(() => { anchoringScroll = false; });
      }
      return;
    }
  }

  function visibleMathRange(math) {
    const visual = math.querySelector('.katex-html') || math;
    const walker = document.createTreeWalker(visual, NodeFilter.SHOW_TEXT);
    let first = null, last = null, node;
    while ((node = walker.nextNode())) if (node.textContent.length) { first ||= node; last = node; }
    const range = document.createRange();
    if (first && last) { range.setStart(first, 0); range.setEnd(last, last.textContent.length); }
    else range.selectNodeContents(visual);
    return range;
  }
  function copiedTex(math) {
    const text = document.createElement('span'); text.dataset.copyTex = '';
    const delimiter = math.dataset.display === 'true' ? '$$' : '$';
    text.textContent = delimiter + (math.dataset.latex || '') + delimiter;
    return text;
  }
  function serializeSelectionNode(node) {
    if (node.nodeType === Node.TEXT_NODE) {
      if (node.parentElement?.closest('pre,code,[data-copy-tex],[data-copy-plain]')) return node.textContent;
      return node.textContent.replace(/\s+/g, ' ');
    }
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) return '';
    if (node.nodeType === Node.ELEMENT_NODE) {
      if (node.matches('[data-copy-ignore],.katex-mathml,annotation,[hidden]')) return '';
      if (node.matches('[data-copy-tex]')) return node.textContent;
      if (node.tagName === 'BR') return '\n';
      if (node.tagName === 'DETAILS' && !node.open) return '';
      if (node.tagName === 'TABLE') {
        return [...node.querySelectorAll('tr')].filter(row => row.closest('table') === node)
          .map(row => [...row.children].filter(cell => /^(TD|TH)$/.test(cell.tagName))
            .map(cell => [...cell.childNodes].map(serializeSelectionNode).join('').replace(/\n+$/g, '')).join('\t')).join('\n') + '\n';
      }
      if (node.tagName === 'PRE') return [...node.childNodes].map(serializeSelectionNode).join('') + '\n';
    }
    let result = [...node.childNodes].map(serializeSelectionNode).join('');
    if (node.nodeType === Node.ELEMENT_NODE && /^(P|DIV|ARTICLE|SECTION|BLOCKQUOTE|H[1-6]|LI|UL|OL|DETAILS)$/.test(node.tagName)
      && result && !result.endsWith('\n')) result += '\n';
    return result;
  }
  function selectionFragment() {
    const selection = getSelection();
    if (!selection || !selection.rangeCount || selection.isCollapsed || !activeSelection()) return null;
    const range = selection.getRangeAt(0);
    const fragment = range.cloneContents();
    for (const math of messages.querySelectorAll('.math[data-math-id]')) {
      if (!math.getClientRects().length || !range.intersectsNode(math)) continue;
      const target = visibleMathRange(math);
      const complete = range.compareBoundaryPoints(Range.START_TO_START, target) <= 0
        && range.compareBoundaryPoints(Range.END_TO_END, target) >= 0;
      if (!complete) continue;
      const copy = fragment.querySelector(`.math[data-math-id="${math.dataset.mathId}"]`);
      if (copy) copy.replaceWith(copiedTex(math));
      else if (math.contains(range.commonAncestorContainer)) fragment.replaceChildren(copiedTex(math));
    }
    fragment.querySelectorAll('[data-copy-ignore],.katex-mathml,annotation,[hidden]').forEach(node => node.remove());
    fragment.querySelectorAll('details:not([open])').forEach(node => node.remove());
    const ancestor = range.commonAncestorContainer.nodeType === Node.ELEMENT_NODE
      ? range.commonAncestorContainer : range.commonAncestorContainer.parentElement;
    if (ancestor?.closest('pre,code,[data-copy-plain]')) {
      const plain = document.createElement('span'); plain.dataset.copyPlain = '';
      plain.append(fragment); fragment.append(plain);
    }
    return fragment;
  }
  function selectionText() {
    const fragment = selectionFragment();
    return fragment ? fragmentText(fragment) : '';
  }
  function fragmentText(fragment) {
    // A range entirely inside literal user text/code has no structural block separators.
    // Preserve even deliberately selected leading/trailing newlines in that case.
    // 选择完全位于用户原文或代码内部时，不额外添加结构分隔；保留用户明确选中的首尾换行。
    if (fragment.childNodes.length === 1 && fragment.firstChild.nodeType === Node.ELEMENT_NODE
      && fragment.firstChild.hasAttribute('data-copy-plain')) return fragment.firstChild.textContent;
    return serializeSelectionNode(fragment).replace(/^\n+|\n+$/g, '');
  }
  document.addEventListener('copy', event => {
    const fragment = selectionFragment();
    if (!fragment || !event.clipboardData) return;
    const text = fragmentText(fragment);
    if (!text) return;
    event.preventDefault(); event.clipboardData.setData('text/plain', text);
    const html = document.createElement('div'); html.append(fragment);
    event.clipboardData.setData('text/html', html.innerHTML);
  });
  document.addEventListener('selectionchange', () => {
    if (activeSelection()) stopFollowing();
    else scheduleFlush();
  });
  document.addEventListener('pointerdown', event => { if (event.button === 0) pointerSelecting = true; });
  const releasePointer = () => { pointerSelecting = false; if (!activeSelection()) scheduleFlush(); };
  document.addEventListener('pointerup', releasePointer);
  document.addEventListener('pointercancel', releasePointer);
  document.addEventListener('pointermove', event => { if (!(event.buttons & 1)) releasePointer(); });
  window.addEventListener('blur', () => { clearSelection(); });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) for (const entry of liveElapsedEntries) refreshElapsedText(entry);
    synchronizeElapsedTimer();
  });
  window.addEventListener('pagehide', () => { if (elapsedTimer !== null) clearInterval(elapsedTimer); elapsedTimer = null; });
  window.addEventListener('scroll', () => {
    // Clearing the old DOM may queue a scroll event until after the new long chat is rendered.
    // Keep explicit navigation alive until its first bottom frame; real upward input cancels it immediately.
    // 清空旧 DOM 产生的滚动事件可能在新长聊天渲染后才到达；首个底部帧前保留显式导航，真实上滚输入立即撤销。
    if (!applying && !localizingUi && !anchoringScroll && !bottomNavigationPending)
      following = !activeSelection() && !pointerSelecting && atBottom();
    updateJumpButton();
  }, { passive: true });
  window.addEventListener('wheel', event => { if (event.deltaY < 0) stopFollowing(); }, { passive: true });
  window.addEventListener('keydown', event => { if (['ArrowUp', 'PageUp', 'Home'].includes(event.key)) stopFollowing(); });
  messages.addEventListener('click', event => {
    const link = event.target.closest('a[href]');
    if (!link) return;
    event.preventDefault();
    try { const url = new URL(link.getAttribute('href')); if (['http:', 'https:', 'mailto:'].includes(url.protocol)) send({ type: 'link', conversationId, url: url.href }); } catch { }
  });
  new ResizeObserver(() => { updateJumpButton(); followBottom(); }).observe(messages);
  window.addEventListener('resize', () => { updateJumpButton(); followBottom(); });
  window.chrome?.webview?.addEventListener('message', event => {
    const command = event.data;
    if (command?.type === 'initializeUi') initializeUi(command);
    else if (command?.type === 'render') applyTranscript(command);
    else if (command?.type === 'openConversation') openConversation(command.conversationId);
    else if (command?.type === 'clearSelection') clearSelection();
    else if (command?.type === 'copyResult') acknowledgeCopy(command);
    else if (command?.type === 'jumpToLatest') jumpToLatest();
    else if (command?.type === 'beforeSend') {
      if (atBottom() && !activeSelection()) { following = true; followBottom(); }
      else stopFollowing();
    }
  });
  window.applyTranscript = applyTranscript;
  window.transcriptSelectionText = selectionText;
  window.transcriptState = () => ({ conversationId, pending: !!pending, following, selection: activeSelection(), messageCount: entries.size,
    cachedConversations: conversations.size, cachedCharacters, cachedNodes,
    liveElapsedCount: liveElapsedEntries.size, elapsedTimerActive: elapsedTimer !== null });
  send({ type: 'ready' });
})();
