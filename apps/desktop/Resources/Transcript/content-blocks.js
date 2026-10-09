/* Content blocks add local copy controls without changing the rendered source or chat records.
 * 内容块只添加本地复制控件，不改写渲染源码或正式聊天记录。 */
(() => {
  'use strict';
  const MAX_PENDING_COPIES = 32;
  const COPY_FEEDBACK_MS = 2200;
  const COPY_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
  const SUCCESS_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
  const EXCLUDED_BLOCKS = '.mermaid-block, .mermaid-source, .math-source, [data-copy-ignore]';
  const strings = {
    copyBlock: '复制此内容块', copyCurrentContent: '复制当前内容', blockPlainText: '文本',
    copied: '已复制', blockCopyFailed: '复制失败，请重试。'
  };
  const states = new WeakMap();
  const records = new Set();
  const pendingCopies = new Map();
  const requestNonce = Math.random().toString(36).slice(2) + '-' + Date.now().toString(36);
  let hooks = {}, copySequence = 0;

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
    const feedback = status === 'error' ? translated('blockCopyFailed')
      : status === 'success' ? translated('copied') : '';
    if (state.feedback.textContent !== feedback) state.feedback.textContent = feedback;
    state.feedback.removeAttribute('title');
    state.feedback.dataset.copyState = status || '';
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
  function createState(pre, code) {
    let block = pre.parentElement;
    if (!block.classList.contains('content-block')) {
      block = document.createElement('div'); block.className = 'content-block';
      // A one-for-one wrapper preserves the Markdown patcher's top-level block count.
      // 一对一包装保留 Markdown 增量更新器的顶层块数量。
      pre.replaceWith(block); block.append(pre);
    }
    const header = document.createElement('header'); header.className = 'content-block-header'; header.dataset.copyIgnore = '';
    const label = document.createElement('span'); label.className = 'content-block-label';
    const feedback = document.createElement('span'); feedback.className = 'content-block-status';
    feedback.setAttribute('role', 'status'); feedback.setAttribute('aria-live', 'polite'); feedback.setAttribute('aria-atomic', 'true');
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'copy-block'; copy.innerHTML = COPY_ICON;
    header.append(label, feedback, copy); block.prepend(header);
    const state = { block, pre, code, header, label, feedback, copy, copyTimer: 0, copyRequestId: null };
    copy.addEventListener('pointerdown', event => {
      // Mouse activation must not collapse a reading selection; keyboard focus remains native.
      // 鼠标点击不能清除正在阅读的选区，键盘焦点仍由原生按钮行为处理。
      if (event.pointerType === 'mouse' && event.button === 0) event.preventDefault();
    });
    copy.addEventListener('click', () => requestCopy(state));
    states.set(block, state); records.add(state); localize(state);
    return state;
  }
  function hydrate(root = document) {
    disposeDetached();
    const codes = [...(root.matches?.('pre > code') ? [root] : []), ...root.querySelectorAll('pre > code')];
    for (const code of codes) {
      const pre = code.parentElement;
      if (!pre.isConnected || pre.firstElementChild !== code || pre.closest(EXCLUDED_BLOCKS)) continue;
      const block = pre.parentElement;
      const state = states.get(block) || createState(pre, code);
      // Cached DOM retains its original listeners and weak state while detached from this view.
      // 缓存 DOM 脱离当前界面时保留原有事件和弱引用状态，恢复时只重新登记与本地化。
      records.add(state); localize(state);
    }
  }
  function requestCopy(state) {
    if (!state.block.isConnected || state.code.parentElement !== state.pre) return;
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
    try { hooks.requestCopy(source, state.block, requestId); }
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
      cancelPending(state); resetFeedback(state); records.delete(state);
    }
  }
  function setLanguage(next) {
    const nextStrings = next?.strings || next || {};
    Object.assign(strings, nextStrings);
    if (typeof nextStrings.copyFailed === 'string') strings.blockCopyFailed = nextStrings.copyFailed;
    for (const state of records) if (state.block.isConnected) localize(state);
  }
  window.kynxaContentBlocks = { configure, hydrate, setLanguage, acknowledgeCopy, resetCopies, disposeDetached };
})();
