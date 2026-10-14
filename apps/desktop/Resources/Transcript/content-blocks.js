/* Content blocks add local copy and reading controls without changing source or chat records.
 * 内容块只添加本地复制与阅读控件，不改写源码或正式聊天记录。 */
(() => {
  'use strict';
  const MAX_PENDING_COPIES = 32;
  const COPY_FEEDBACK_MS = 2200;
  const READING_NOTICE_MS = 3000;
  const LONG_BLOCK_LINES = 24;
  const LONG_BLOCK_HEIGHT_PX = 520;
  const PREVIEW_LINES = 12;
  const MAX_SAVED_READING_BLOCKS = 256;
  const COPY_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
  const SUCCESS_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
  const EXCLUDED_BLOCKS = '.mermaid-block, .mermaid-source, .math-source, [data-copy-ignore]';
  const strings = {
    copyBlock: '复制此内容块', copyCurrentContent: '复制当前内容', blockPlainText: '文本',
    copied: '已复制', blockCopyFailed: '复制失败，请重试。',
    blockWrap: '自动换行', blockUnwrap: '原样显示', blockExpand: '展开', blockCollapse: '收起',
    blockLineCount: '{0} 行', blockClearSelection: '请先取消文字选择，再调整显示。'
  };
  const states = new WeakMap();
  const records = new Set();
  const pendingCopies = new Map();
  // A body owns only small preferences; cached conversations retain them without a second content cache.
  // 正文仅拥有少量阅读偏好；聊天缓存恢复时复用这些偏好，不另存一份聊天内容。
  const readingPreferences = new WeakMap();
  const requestNonce = Math.random().toString(36).slice(2) + '-' + Date.now().toString(36);
  let hooks = {}, copySequence = 0;
  const readingResizeObserver = typeof ResizeObserver === 'function' ? new ResizeObserver(entries => {
    for (const entry of entries) {
      const state = states.get(entry.target);
      if (!state || !entry.target.isConnected || state.measuredWidth === entry.contentRect.width) continue;
      state.measuredWidth = entry.contentRect.width;
      refreshReading(state);
    }
  }) : null;

  function configure(options) { hooks = { ...hooks, ...options }; }
  function translated(key) { return strings[key] || key; }
  function copyLabel(state) {
    if (state.copy.dataset.copyState === 'success') return translated('copied');
    if (state.copy.dataset.copyState === 'error') return translated('blockCopyFailed');
    return translated(state.pre.dataset.codeComplete === 'false' ? 'copyCurrentContent' : 'copyBlock');
  }
  function localize(state) {
    // Labels are untrusted model text; textContent preserves them as text, never markup.
    // 标签来自不可信模型文本，使用 textContent 仅显示文字，绝不解释为标记。
    const label = state.code.dataset.language || translated('blockPlainText');
    if (state.label.textContent !== label) state.label.textContent = label;
    state.label.title = state.label.textContent;
    const status = state.copy.dataset.copyState;
    const description = copyLabel(state);
    // Success already has inline text; avoid a second floating native title tooltip.
    // 成功已有框内小字，无需再弹出重复的原生 title 提示。
    if (status) state.copy.removeAttribute('title');
    else state.copy.title = description;
    state.copy.setAttribute('aria-label', description);
    const feedback = state.readingNotice ? translated('blockClearSelection') : status === 'error' ? translated('blockCopyFailed')
      : status === 'success' ? translated('copied') : '';
    if (state.feedback.textContent !== feedback) state.feedback.textContent = feedback;
    state.feedback.removeAttribute('title');
    state.feedback.dataset.copyState = status || '';
    state.feedback.dataset.readingState = state.readingNotice ? 'blocked' : '';
    const wrapDescription = translated('blockWrap');
    if (state.wrap.textContent !== wrapDescription) state.wrap.textContent = wrapDescription;
    state.wrap.setAttribute('aria-label', wrapDescription);
    state.wrap.setAttribute('aria-pressed', String(state.preferences.wrap));
    state.wrap.title = translated(state.preferences.wrap ? 'blockUnwrap' : 'blockWrap');
    const foldDescription = translated(state.block.dataset.collapsed === 'true' ? 'blockExpand' : 'blockCollapse');
    if (state.fold.textContent !== foldDescription) state.fold.textContent = foldDescription;
    state.fold.setAttribute('aria-label', foldDescription);
    state.fold.setAttribute('aria-expanded', String(state.block.dataset.collapsed !== 'true'));
    const countDescription = translated('blockLineCount').replace('{0}', String(state.lineCount));
    if (state.lines.textContent !== countDescription) state.lines.textContent = countDescription;
    state.lines.dataset.lineCount = String(state.lineCount);
  }
  function countLines(source) {
    if (!source) return 0;
    let count = 1;
    for (let index = 0; index < source.length; index++) {
      if (source[index] === '\n') count++;
      else if (source[index] === '\r') { count++; if (source[index + 1] === '\n') index++; }
    }
    return count;
  }
  function refreshReading(state) {
    const complete = state.pre.dataset.codeComplete !== 'false';
    if (!complete) state.preferences.collapsed = false;
    state.block.dataset.wrap = String(state.preferences.wrap);
    state.block.dataset.collapsed = String(complete && state.preferences.collapsed);
    const style = getComputedStyle(state.pre);
    const lineHeight = parseFloat(style.lineHeight) || 25.5;
    const padding = (parseFloat(style.paddingTop) || 0) + (parseFloat(style.paddingBottom) || 0);
    // The clipped preview has no bottom padding; account for its horizontal scrollbar instead.
    // 裁切预览不保留底部内边距，改为计入横向滚动条厚度，避免露出下一行的半截文字。
    const scrollbarHeight = Math.max(0, state.pre.offsetHeight - state.pre.clientHeight);
    state.block.style.setProperty('--content-block-preview-height', (lineHeight * PREVIEW_LINES + padding + scrollbarHeight) + 'px');
    state.isLong = state.lineCount > LONG_BLOCK_LINES || state.pre.scrollHeight > LONG_BLOCK_HEIGHT_PX;
    // An open fence always shows all received content; completion only exposes a manual fold action.
    // 未闭合围栏始终展示已接收的全部内容；围栏完整时仅提供手动收起，不自动折叠。
    state.fold.hidden = !complete || !state.isLong;
    if (state.fold.hidden && state.block.dataset.collapsed === 'true') {
      state.preferences.collapsed = false; state.block.dataset.collapsed = 'false';
    }
    localize(state);
  }
  function showReadingNotice(state) {
    clearTimeout(state.readingTimer); state.readingNotice = true; localize(state);
    state.readingTimer = setTimeout(() => {
      state.readingTimer = 0; state.readingNotice = false; localize(state);
    }, READING_NOTICE_MS);
  }
  function changeReading(state, action) {
    if (!state.block.isConnected || state.code.parentElement !== state.pre) return;
    if (action === 'fold' && state.fold.hidden) return;
    if (hooks.beforeLayoutChange?.(state.block) === false) { showReadingNotice(state); return; }
    clearTimeout(state.readingTimer); state.readingTimer = 0; state.readingNotice = false;
    if (action === 'wrap') {
      if (!state.preferences.wrap) state.preferences.scrollLeft = state.pre.scrollLeft;
      state.preferences.wrap = !state.preferences.wrap;
    } else state.preferences.collapsed = !state.preferences.collapsed;
    refreshReading(state);
    if (action === 'wrap') state.pre.scrollLeft = state.preferences.wrap ? 0 : state.preferences.scrollLeft || 0;
    hooks.afterLayoutChange?.(state.block);
  }
  function createReadingButton(action) {
    const button = document.createElement('button'); button.type = 'button';
    button.className = 'content-block-reading-action'; button.dataset.blockAction = action;
    return button;
  }
  function resetFeedback(state) {
    clearTimeout(state.copyTimer); state.copyTimer = 0;
    delete state.copy.dataset.copyState;
    state.copy.innerHTML = COPY_ICON;
    localize(state);
  }
  function cancelPending(state) {
    if (state.copyRequestId) pendingCopies.delete(state.copyRequestId);
    state.copyRequestId = null;
  }
  function showFeedback(state, success) {
    clearTimeout(state.copyTimer);
    state.copy.dataset.copyState = success ? 'success' : 'error';
    state.copy.innerHTML = success ? SUCCESS_ICON : COPY_ICON;
    localize(state);
    state.copyTimer = setTimeout(() => resetFeedback(state), COPY_FEEDBACK_MS);
  }
  function createState(pre, code, preferences) {
    let block = pre.parentElement;
    if (!block.classList.contains('content-block')) {
      block = document.createElement('div'); block.className = 'content-block';
      // A one-for-one wrapper preserves the Markdown patcher's top-level block count.
      // 一对一包装保留 Markdown 增量更新器的顶层块数量。
      pre.replaceWith(block); block.append(pre);
    }
    const header = document.createElement('header'); header.className = 'content-block-header'; header.dataset.copyIgnore = '';
    const label = document.createElement('span'); label.className = 'content-block-label';
    const lines = document.createElement('span'); lines.className = 'content-block-lines';
    const wrap = createReadingButton('wrap'), fold = createReadingButton('fold');
    const feedback = document.createElement('span'); feedback.className = 'content-block-status';
    feedback.setAttribute('role', 'status'); feedback.setAttribute('aria-live', 'polite'); feedback.setAttribute('aria-atomic', 'true');
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'copy-block'; copy.innerHTML = COPY_ICON;
    header.append(label, lines, wrap, fold, copy, feedback); block.prepend(header);
    const state = { block, pre, code, header, label, lines, wrap, fold, feedback, copy, preferences,
      lineCount: countLines(code.textContent), isLong: false, measuredWidth: null,
      copyTimer: 0, copyRequestId: null, readingTimer: 0, readingNotice: false };
    for (const button of [copy, wrap, fold]) {
      button.addEventListener('pointerdown', event => {
        // Mouse activation preserves a reading range so layout hooks can reject rather than erase it.
        // 鼠标操作保留阅读选区，使布局钩子能够明确拒绝操作，而非悄悄清除选区。
        if (event.pointerType === 'mouse' && event.button === 0) event.preventDefault();
      });
    }
    copy.addEventListener('click', () => requestCopy(state));
    wrap.addEventListener('click', () => changeReading(state, 'wrap'));
    fold.addEventListener('click', () => changeReading(state, 'fold'));
    states.set(block, state); records.add(state);
    return state;
  }
  function hydrate(root = document) {
    disposeDetached();
    const codes = [...(root.matches?.('pre > code') ? [root] : []), ...root.querySelectorAll('pre > code')];
    const bodyIndices = new Map();
    for (const code of codes) {
      const pre = code.parentElement;
      if (!pre.isConnected || pre.firstElementChild !== code || pre.closest(EXCLUDED_BLOCKS)) continue;
      const block = pre.parentElement;
      const body = pre.closest('.message-body') || root;
      const index = bodyIndices.get(body) || 0; bodyIndices.set(body, index + 1);
      let preferences = readingPreferences.get(body);
      if (!preferences) { preferences = new Map(); readingPreferences.set(body, preferences); }
      if (!preferences.has(index)) preferences.set(index, { wrap: false, collapsed: false, scrollLeft: 0 });
      const existing = states.get(block);
      const state = existing || createState(pre, code, preferences.get(index));
      // Cached DOM retains its original listeners and weak state while detached from this view.
      // 缓存 DOM 脱离当前界面时保留原有事件和弱引用状态，恢复时只重新登记与本地化。
      records.add(state); refreshReading(state); readingResizeObserver?.observe(state.block);
      if (!existing && !state.preferences.wrap) state.pre.scrollLeft = state.preferences.scrollLeft || 0;
    }
  }
  function captureReading(root) {
    const saved = [];
    const bodies = [...root.querySelectorAll('.message-body')];
    for (let bodyIndex = 0; bodyIndex < bodies.length; bodyIndex++) {
      const body = bodies[bodyIndex], preferences = readingPreferences.get(body);
      if (!preferences) continue;
      const activeStates = new Map([...body.querySelectorAll('.content-block')]
        .map(block => states.get(block)).filter(Boolean).map(state => [state.preferences, state]));
      for (const [blockIndex, preference] of preferences) {
        const state = activeStates.get(preference);
        const scrollLeft = Math.max(0, state && !preference.wrap ? state.pre.scrollLeft : preference.scrollLeft || 0);
        if (!preference.wrap && !preference.collapsed && !scrollLeft) continue;
        // Virtual rows retain only bounded scalar preferences, never source copies or detached DOM.
        // 虚拟行仅保留有界的标量偏好，不复制源码，也不持有已卸载 DOM。
        saved.push({ bodyIndex, blockIndex, wrap: preference.wrap === true, collapsed: preference.collapsed === true, scrollLeft });
        if (saved.length >= MAX_SAVED_READING_BLOCKS) return saved;
      }
    }
    return saved;
  }
  function restoreReading(root, saved) {
    if (!Array.isArray(saved)) return;
    const bodies = [...root.querySelectorAll('.message-body')];
    for (const item of saved.slice(0, MAX_SAVED_READING_BLOCKS)) {
      if (!Number.isSafeInteger(item.bodyIndex) || !Number.isSafeInteger(item.blockIndex) || item.blockIndex < 0) continue;
      const body = bodies[item.bodyIndex];
      if (!body) continue;
      let preferences = readingPreferences.get(body);
      if (!preferences) { preferences = new Map(); readingPreferences.set(body, preferences); }
      preferences.set(item.blockIndex, { wrap: item.wrap === true, collapsed: item.collapsed === true,
        scrollLeft: Number.isFinite(item.scrollLeft) ? Math.max(0, item.scrollLeft) : 0 });
    }
  }
  function requestCopy(state) {
    if (!state.block.isConnected || state.code.parentElement !== state.pre) return;
    clearTimeout(state.readingTimer); state.readingTimer = 0; state.readingNotice = false;
    cancelPending(state); resetFeedback(state);
    if (typeof hooks.requestCopy !== 'function') { showFeedback(state, false); return; }
    const requestId = 'block-' + requestNonce + '-' + (++copySequence);
    while (pendingCopies.size >= MAX_PENDING_COPIES) {
      const firstId = pendingCopies.keys().next().value;
      const previous = pendingCopies.get(firstId);
      pendingCopies.delete(firstId);
      if (previous.copyRequestId === firstId) previous.copyRequestId = null;
    }
    state.copyRequestId = requestId; pendingCopies.set(requestId, state);
    // Read a click-time snapshot, including incomplete streaming text, without normalizing whitespace.
    // 点击时读取快照，包含流式输出的当前文本，不归一化空白、缩进或换行。
    const source = state.code.textContent;
    try {
      if (hooks.requestCopy(source, state.block, requestId) === false) {
        cancelPending(state); showFeedback(state, false);
      }
    }
    catch {
      if (state.copyRequestId === requestId && state.block.isConnected) {
        cancelPending(state); showFeedback(state, false);
      }
    }
  }
  function acknowledgeCopy(command) {
    const state = pendingCopies.get(command?.requestId);
    pendingCopies.delete(command?.requestId);
    if (!state || !state.block.isConnected || state.copyRequestId !== command.requestId) return;
    state.copyRequestId = null;
    // Only the native clipboard receipt can report success; old instances cannot affect new blocks.
    // 仅原生剪贴板回执能够报告成功，旧实例回执不能影响新的内容块。
    showFeedback(state, command.success === true);
  }
  function resetCopies() {
    pendingCopies.clear();
    for (const state of records) { state.copyRequestId = null; resetFeedback(state); }
  }
  function disposeDetached() {
    for (const state of records) {
      if (state.block.isConnected) continue;
      cancelPending(state); clearTimeout(state.readingTimer); state.readingTimer = 0; state.readingNotice = false;
      resetFeedback(state); readingResizeObserver?.unobserve(state.block); records.delete(state);
    }
  }
  function setLanguage(next) {
    const nextStrings = next?.strings || next || {};
    Object.assign(strings, nextStrings);
    if (typeof nextStrings.copyFailed === 'string') strings.blockCopyFailed = nextStrings.copyFailed;
    for (const state of records) if (state.block.isConnected) localize(state);
  }
  function reveal(block) {
    const state = states.get(block);
    if (!state || !block.isConnected) return false;
    if (state.block.dataset.collapsed !== 'true') return true;
    if (hooks.beforeLayoutChange?.(block) === false) { showReadingNotice(state); return false; }
    // Search expands only on explicit result navigation and persists the same reading preference.
    // 搜索仅在主动跳转结果时展开内容，并复用原有阅读偏好，不创建临时源码副本。
    state.preferences.collapsed = false; refreshReading(state); hooks.afterLayoutChange?.(block);
    return true;
  }
  window.kynxaContentBlocks = { configure, hydrate, setLanguage, acknowledgeCopy, resetCopies, disposeDetached, reveal,
    captureReading, restoreReading };
})();
