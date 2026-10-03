/* One browser selection surface for the whole conversation. KaTeX stays real DOM. */
(() => {
  'use strict';
  const messages = document.getElementById('messages');
  const uiStrings = {
    conversation: '对话', transcript: '聊天记录', copy: '复制', copyMessage: '复制整条消息', retry: '重试',
    reasoning: '思考过程', thinking: '正在思考…', reasoningDuration: '思考过程 · {0} 秒', stopped: '已停止生成',
    interrupted: '回复中断，请重试。', replying: '正在回复…', generating: '正在生成',
    toolActivities: '工具活动', toolRunning: '执行中', toolCompleted: '已完成', toolError: '工具失败',
    toolDenied: '已拒绝', toolApproval: '等待批准', toolCancelled: '已取消', toolUnknown: '结果未知',
    toolSearchWeb: '搜索资料', toolReadWeb: '阅读网页', toolReadFile: '读取文件', toolInspectFile: '查看文件',
    toolListFiles: '查看文件夹', toolSearchFiles: '查找文件', toolEditFile: '修改文件', toolDeleteFile: '删除文件',
    toolCreateFolder: '创建文件夹', toolRunCommand: '运行命令', toolUseSkill: '使用技能', toolReadSkill: '读取技能',
    toolFindSkill: '查找技能', toolInspectSkill: '检查技能', toolFindTools: '查找工具', toolReadResult: '读取工具记录',
    toolFindHistory: '查找聊天记录', toolReadHistory: '读取聊天记录', toolExecute: '执行操作',
    toolOutcomeUncertain: '操作已中断，执行结果尚未确定。', toolTimedOut: '操作超时。',
    toolSandboxUnavailable: '沙箱暂不可用。', toolSkillUnavailable: '技能运行环境尚未满足。', toolApprovalExpired: '批准已过期。',
    toolCommandUnavailable: '此命令暂不支持。', toolConnectionUnavailable: '工具连接不可用。', toolAuthRequired: '工具需要认证。',
    toolMoreWebLinks: '另{0}个链接',
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
  const send = value => window.chrome?.webview?.postMessage(value);
  const presentationFor = message => window.KynxaMessagePresentation.selectMessagePresentation(message);
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
  function localizeEntry(entry) {
    entry.copy.title = uiStrings.copy;
    entry.copy.setAttribute('aria-label', uiStrings.copyMessage);
    entry.retry.textContent = uiStrings.retry;
    if (entry.message) {
      setStatusText(entry, statusText(entry.message));
      setToolText(entry.elapsed, entry.presentation?.mode === 'final'
        ? window.KynxaMessagePresentation.elapsedText(entry.message.durationMs, uiStrings) : '');
    }
    entry.status.querySelector('.streaming-dot')?.setAttribute('aria-label', uiStrings.generating);
    for (const row of entry.toolRows?.values() || []) localizeToolRow(row);
    for (const row of entry.segmentRows?.values() || []) {
      for (const tool of row.toolRows.values()) localizeToolRow(tool);
    }
  }
  function initializeUi(command) {
    const left = scrollX, top = scrollY;
    // Startup has no content to preserve; let its first render follow the bottom.
    localizingUi = entries.size > 0;
    if (localizingUi) cancelAnimationFrame(scrollFrame);
    cancelAnimationFrame(languageFrame);
    for (const key of Object.keys(uiStrings))
      if (typeof command.strings?.[key] === 'string') uiStrings[key] = command.strings[key];
    document.documentElement.lang = command.language === 'en' ? 'en' : 'zh-CN';
    document.title = uiStrings.conversation;
    messages.setAttribute('aria-label', uiStrings.transcript);
    // Localize only application controls; message DOM and browser selections stay intact.
    for (const entry of entries.values()) localizeEntry(entry);
    for (const saved of conversations.values()) for (const entry of saved.entries.values()) localizeEntry(entry);
    if (!localizingUi) return;
    // Labels may wrap differently. Keep the user's scroll position and suppress only
    // the automatic bottom-follow triggered by this layout, without touching ranges.
    if (scrollX !== left || scrollY !== top) scrollTo({ left, top, behavior: 'instant' });
    languageFrame = requestAnimationFrame(() => {
      languageFrame = requestAnimationFrame(() => { localizingUi = false; });
    });
  }
  const atBottom = () => document.documentElement.scrollHeight - innerHeight - scrollY <= 36;
  const activeSelection = () => {
    const selection = getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return false;
    try { return selection.getRangeAt(0).intersectsNode(messages); } catch { return false; }
  };
  function followBottom() {
    if (localizingUi || !following || activeSelection() || pointerSelecting) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = requestAnimationFrame(() => {
      if (!localizingUi && following && !activeSelection() && !pointerSelecting) scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' });
    });
  }
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
    copy.innerHTML = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
    const retry = document.createElement('button'); retry.type = 'button'; retry.className = 'retry'; retry.textContent = uiStrings.retry;
    const tools = document.createElement('div'); tools.className = 'tool-activities'; tools.hidden = true;
    const timeline = document.createElement('div'); timeline.className = 'assistant-timeline'; timeline.hidden = true;
    const entry = { article, content, elapsed, body, status, actions, copy, retry,
      tools, toolRows: new Map(), timeline, segmentRows: new Map(), message: null, presentation: null };
    copy.addEventListener('click', () => send({ type: 'copy', conversationId, text: copiedMessage(entry.message) }));
    retry.addEventListener('click', () => send({ type: 'retry', conversationId, id: entry.message.id }));
    actions.append(copy, retry); content.append(elapsed, timeline, body, status); article.append(content, actions);
    return entry;
  }
  function updateMessage(entry, message) {
    const old = entry.message;
    if (old && ['role', 'content', 'html', 'reasoningHtml', 'reasoningTitle', 'reasoningState', 'reasoningSeconds', 'status', 'waiting', 'streaming', 'error', 'canRetry', 'durationMs', 'presentationMode']
      .every(key => old[key] === message[key]) && !old.toolActivities?.length && !message.toolActivities?.length &&
      !old.assistantSegments?.length && !message.assistantSegments?.length) return;
    entry.message = message;
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
    const elapsed = presentation.mode === 'final' ? window.KynxaMessagePresentation.elapsedText(message.durationMs, uiStrings) : '';
    setToolText(entry.elapsed, elapsed); entry.elapsed.hidden = !elapsed;
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
    for (const [id, row] of entry.segmentRows) if (!ids.has(id)) { row.root.remove(); entry.segmentRows.delete(id); }
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
    const row = { root, title, state, command, error, links, more, moreCount: 0, website, tool: null, presentation: null, signature: null, count: 1 };
    return row;
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
      const signature = JSON.stringify(group.views.map(view => view.signature));
      if (row.signature === signature) continue;
      row.signature = signature;
      row.tool = first.tool; row.presentation = first.presentation; row.count = group.views.length;
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
      const action = [...new Set(group.views.map(view => view.presentation.action).filter(Boolean))].join('\n');
      setToolText(row.command, action); row.command.hidden = !action; row.command.title = action;
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
    pending = null; clearSelection(); following = true;
    // Detach complete DOM trees: returning to a chat reuses KaTeX, code highlighting
    // and expanded reasoning instead of rebuilding every element.
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
    followBottom();
  }
  function applyTranscript(snapshot) {
    if (!snapshot || !Array.isArray(snapshot.messages)) return;
    const changedConversation = conversationId !== snapshot.conversationId;
    if (changedConversation) openConversation(snapshot.conversationId);
    if (changedConversation || snapshot.openAtBottom) {
      pending = null; clearSelection(); following = true;
    } else if (activeSelection()) { pending = snapshot; return; }
    const anchors = following ? [] : scrollAnchors();
    applying = true;
    const ids = new Set(snapshot.messages.map(message => String(message.id)));
    for (const [id, entry] of entries) if (!ids.has(id)) { entry.article.remove(); entries.delete(id); }
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
    followBottom();
    document.fonts.ready.then(followBottom);
  }
  function scrollAnchors() {
    const anchors = [], fallbacks = [];
    // Prefer a surviving paragraph in view; keep message-level fallbacks when transient steps disappear.
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
    if (activeSelection()) following = false;
    else scheduleFlush();
  });
  document.addEventListener('pointerdown', event => { if (event.button === 0) pointerSelecting = true; });
  const releasePointer = () => { pointerSelecting = false; if (!activeSelection()) scheduleFlush(); };
  document.addEventListener('pointerup', releasePointer);
  document.addEventListener('pointercancel', releasePointer);
  document.addEventListener('pointermove', event => { if (!(event.buttons & 1)) releasePointer(); });
  window.addEventListener('blur', () => { clearSelection(); });
  window.addEventListener('scroll', () => {
    if (!applying && !localizingUi && !anchoringScroll) following = !activeSelection() && !pointerSelecting && atBottom();
  }, { passive: true });
  window.addEventListener('wheel', event => { if (event.deltaY < 0) following = false; }, { passive: true });
  window.addEventListener('keydown', event => { if (['ArrowUp', 'PageUp', 'Home'].includes(event.key)) following = false; });
  messages.addEventListener('click', event => {
    const link = event.target.closest('a[href]');
    if (!link) return;
    event.preventDefault();
    try { const url = new URL(link.getAttribute('href')); if (['http:', 'https:', 'mailto:'].includes(url.protocol)) send({ type: 'link', conversationId, url: url.href }); } catch { }
  });
  new ResizeObserver(followBottom).observe(messages);
  window.chrome?.webview?.addEventListener('message', event => {
    const command = event.data;
    if (command?.type === 'initializeUi') initializeUi(command);
    else if (command?.type === 'render') applyTranscript(command);
    else if (command?.type === 'openConversation') openConversation(command.conversationId);
    else if (command?.type === 'clearSelection') clearSelection();
    else if (command?.type === 'beforeSend') { following = atBottom() && !activeSelection(); followBottom(); }
  });
  window.applyTranscript = applyTranscript;
  window.transcriptSelectionText = selectionText;
  window.transcriptState = () => ({ conversationId, pending: !!pending, following, selection: activeSelection(), messageCount: entries.size,
    cachedConversations: conversations.size, cachedCharacters, cachedNodes });
  send({ type: 'ready' });
})();
