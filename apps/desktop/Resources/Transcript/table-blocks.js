/* Table actions copy a click-time projection; rendered cells and formal Markdown stay unchanged.
 * 表格操作复制点击时的内容投影，不改写已渲染单元格或正式 Markdown。 */
(() => {
  'use strict';
  const MAX_PENDING_COPIES = 32;
  const MAX_GRID_CELLS = 200000;
  const MAX_GRID_COLUMNS = 2048;
  const COPY_FEEDBACK_MS = 2200;
  const COPY_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
  const SUCCESS_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
  const OPTIONS_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m7 10 5 5 5-5"/></svg>';
  const strings = {
    tableBlockLabel: '表格', copyTable: '复制表格（可粘贴到 Excel）', copyTableMarkdown: '复制 Markdown 表格',
    tableCopyOptions: '表格复制选项', tableEmpty: '空表格，暂无可复制内容', copied: '已复制', copyFailed: '复制失败，请重试。'
  };
  const states = new WeakMap();
  const records = new Set();
  const pendingCopies = new Map();
  const requestNonce = Math.random().toString(36).slice(2) + '-' + Date.now().toString(36);
  let hooks = {}, copySequence = 0, menuSequence = 0, openMenuState = null;

  function configure(options) { hooks = { ...hooks, ...options }; }
  function translated(key) { return strings[key] || key; }
  function tableRows(table) { return [...table.rows].filter(row => row.closest('table') === table); }
  function hasCells(table) { return tableRows(table).some(row => row.cells.length > 0); }
  function localize(state) {
    state.label.textContent = translated('tableBlockLabel');
    const status = state.copy.dataset.copyState;
    const copyLabel = status === 'success' ? translated('copied') : status === 'error'
      ? translated('copyFailed') : translated(state.copy.disabled ? 'tableEmpty' : 'copyTable');
    state.copy.setAttribute('aria-label', copyLabel);
    if (status) state.copy.removeAttribute('title');
    else state.copy.title = copyLabel;
    state.options.setAttribute('aria-label', translated('tableCopyOptions'));
    state.options.title = translated('tableCopyOptions');
    state.markdown.textContent = translated('copyTableMarkdown');
    state.markdown.setAttribute('aria-label', translated(state.markdown.disabled ? 'tableEmpty' : 'copyTableMarkdown'));
    state.feedback.textContent = status === 'success' ? translated('copied') : status === 'error' ? translated('copyFailed') : '';
    state.feedback.dataset.copyState = status || '';
  }
  function setMenuOpen(state, isOpen, restoreFocus = false) {
    if (isOpen && openMenuState && openMenuState !== state) setMenuOpen(openMenuState, false);
    state.menu.hidden = !isOpen;
    state.options.setAttribute('aria-expanded', String(isOpen));
    openMenuState = isOpen ? state : openMenuState === state ? null : openMenuState;
    if (restoreFocus && state.options.isConnected) state.options.focus({ preventScroll: true });
  }
  function dismissMenu() {
    if (!openMenuState) return false;
    setMenuOpen(openMenuState, false, true);
    return true;
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
  function preserveMouseSelection(event) {
    if (event.pointerType === 'mouse' && event.button === 0) event.preventDefault();
  }
  function createState(table) {
    let scroll = table.parentElement;
    if (!scroll?.classList.contains('table-scroll')) {
      scroll = document.createElement('div'); scroll.className = 'table-scroll';
      table.replaceWith(scroll); scroll.append(table);
    }
    const block = document.createElement('div'); block.className = 'table-block';
    // One outer wrapper keeps the incremental patcher's top-level source-block correspondence.
    // 单个外层包装维持增量更新器的顶层源码块一一对应关系。
    scroll.replaceWith(block); block.append(scroll);
    const header = document.createElement('header'); header.className = 'table-block-header'; header.dataset.copyIgnore = '';
    const label = document.createElement('span'); label.className = 'table-block-label';
    const feedback = document.createElement('span'); feedback.className = 'table-block-status';
    feedback.setAttribute('role', 'status'); feedback.setAttribute('aria-live', 'polite'); feedback.setAttribute('aria-atomic', 'true');
    const actions = document.createElement('div'); actions.className = 'table-block-actions';
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'copy-table'; copy.dataset.copyFormat = 'tsv'; copy.innerHTML = COPY_ICON;
    const options = document.createElement('button'); options.type = 'button'; options.className = 'table-copy-options'; options.innerHTML = OPTIONS_ICON;
    options.setAttribute('aria-haspopup', 'menu'); options.setAttribute('aria-expanded', 'false');
    const menu = document.createElement('div'); menu.className = 'table-copy-menu'; menu.id = 'table-copy-menu-' + (++menuSequence);
    menu.hidden = true; menu.setAttribute('role', 'menu'); options.setAttribute('aria-controls', menu.id);
    options.id = menu.id + '-toggle'; menu.setAttribute('aria-labelledby', options.id);
    const markdown = document.createElement('button'); markdown.type = 'button'; markdown.className = 'copy-table-markdown';
    markdown.dataset.copyFormat = 'markdown'; markdown.setAttribute('role', 'menuitem'); menu.append(markdown);
    actions.append(copy, options, menu); header.append(label, feedback, actions); block.prepend(header);
    const state = { block, table, scroll, header, label, feedback, copy, options, menu, markdown, copyTimer: 0, copyRequestId: null };
    for (const control of [copy, options, markdown]) control.addEventListener('pointerdown', preserveMouseSelection);
    copy.addEventListener('click', () => requestCopy(state, 'tsv'));
    options.addEventListener('click', event => {
      const isOpen = state.menu.hidden;
      setMenuOpen(state, isOpen);
      if (isOpen && event.detail === 0) state.markdown.focus({ preventScroll: true });
    });
    options.addEventListener('keydown', event => {
      if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
      event.preventDefault(); setMenuOpen(state, true); state.markdown.focus({ preventScroll: true });
    });
    markdown.addEventListener('click', event => {
      setMenuOpen(state, false, event.detail === 0); requestCopy(state, 'markdown');
    });
    states.set(table, state); records.add(state);
    return state;
  }
  function hydrate(root = document) {
    disposeDetached();
    const tables = [...(root.matches?.('table') ? [root] : []), ...root.querySelectorAll('table')];
    for (const table of tables) {
      if (!table.isConnected || table.closest('[data-copy-ignore],.mermaid-block') || table.parentElement?.closest('table')) continue;
      const state = states.get(table) || createState(table);
      records.add(state);
      const isEmpty = !hasCells(table);
      state.copy.disabled = isEmpty; state.options.disabled = isEmpty; state.markdown.disabled = isEmpty;
      if (isEmpty) setMenuOpen(state, false);
      localize(state);
    }
  }
  function mathSource(math) {
    const delimiter = math.dataset.display === 'true' ? '$$' : '$';
    return delimiter + (math.dataset.latex || '') + delimiter;
  }
  function cellText(cell) {
    const clone = cell.cloneNode(true);
    clone.dataset.copyPlain = '';
    // Project formulas before the shared selection serializer, avoiding duplicate KaTeX visual/mathml text.
    // 先还原公式再使用共用选区序列化器，避免复制 KaTeX 可视层与 MathML 的重复文字。
    for (const math of clone.querySelectorAll('.math[data-latex]')) {
      const source = document.createElement('span'); source.dataset.copyTex = ''; source.textContent = mathSource(math); math.replaceWith(source);
    }
    const text = typeof hooks.serializeNode === 'function' ? hooks.serializeNode(clone) : fallbackText(clone);
    const last = [...clone.childNodes].reverse().find(node => node.nodeType !== Node.TEXT_NODE || node.textContent.trim());
    return last?.nodeType === Node.ELEMENT_NODE && /^(P|DIV|UL|OL|LI|BLOCKQUOTE)$/.test(last.tagName) ? text.replace(/\n$/, '') : text;
  }
  function fallbackText(node) {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent;
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) return '';
    if (node.nodeType === Node.ELEMENT_NODE) {
      if (node.matches('[data-copy-ignore],[hidden],.katex-mathml,annotation,script,style')) return '';
      if (node.tagName === 'BR') return '\n';
    }
    const text = [...node.childNodes].map(fallbackText).join('');
    return node.nodeType === Node.ELEMENT_NODE && /^(P|DIV|LI)$/.test(node.tagName) ? text + '\n' : text;
  }
  function escapeMarkdown(text) {
    return text.replace(/&/g, '&amp;').replace(/\\/g, '\\\\').replace(/\|/g, '\\|').replace(/([`*_\[\]~])/g, '\\$1')
      .replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/\r\n|\r|\n/g, '<br>');
  }
  function markdownNode(node) {
    if (node.nodeType === Node.TEXT_NODE) return escapeMarkdown(node.textContent);
    if (node.nodeType !== Node.ELEMENT_NODE) return '';
    if (node.matches('[data-copy-ignore],[hidden],.katex-mathml,annotation,script,style')) return '';
    if (node.matches('.math[data-latex]')) return mathSource(node).replace(/\|/g, '\\|').replace(/\r\n|\r|\n/g, '<br>');
    if (node.tagName === 'BR') return '<br>';
    if (node.tagName === 'CODE') {
      const text = node.textContent.replace(/\|/g, '\\|').replace(/\r\n|\r|\n/g, '<br>');
      const longestRun = (text.match(/`+/g) || []).reduce((longest, run) => Math.max(longest, run.length), 0);
      const fence = '`'.repeat(longestRun + 1), padding = /^[` ]|[` ]$/.test(text) ? ' ' : '';
      return fence + padding + text + padding + fence;
    }
    const text = [...node.childNodes].map(markdownNode).join('');
    if (node.tagName === 'STRONG' || node.tagName === 'B') return '**' + text + '**';
    if (node.tagName === 'EM' || node.tagName === 'I') return '*' + text + '*';
    if (node.tagName === 'DEL' || node.tagName === 'S') return '~~' + text + '~~';
    if (/^(P|DIV|LI)$/.test(node.tagName)) return text + '<br>';
    return text;
  }
  function cellMarkdown(cell) {
    const text = [...cell.childNodes].map(markdownNode).join('');
    const last = [...cell.childNodes].reverse().find(node => node.nodeType !== Node.TEXT_NODE || node.textContent.trim());
    return last?.nodeType === Node.ELEMENT_NODE && /^(P|DIV|LI)$/.test(last.tagName) ? text.replace(/<br>$/, '') : text;
  }
  function rectangularCells(table, format) {
    const rows = tableRows(table);
    if (rows.length > MAX_GRID_CELLS) throw new Error('table-size');
    const grid = rows.map(() => []);
    const groupEnds = new Map();
    rows.forEach((row, index) => groupEnds.set(row.parentElement, index + 1));
    let columnCount = 0;
    for (let rowIndex = 0; rowIndex < rows.length; rowIndex++) {
      let columnIndex = 0;
      for (const cell of rows[rowIndex].cells) {
        while (grid[rowIndex][columnIndex] !== undefined) columnIndex++;
        const columnSpan = Math.max(1, cell.colSpan);
        const groupEnd = groupEnds.get(rows[rowIndex].parentElement);
        const rowSpan = cell.rowSpan === 0 ? groupEnd - rowIndex : Math.min(cell.rowSpan, groupEnd - rowIndex);
        const nextColumnCount = Math.max(columnCount, columnIndex + columnSpan);
        // Reject pathological spans instead of allocating an unbounded matrix or silently truncating a copy.
        // 异常跨度明确复制失败，避免无限分配矩阵或静默截断复制内容。
        if (nextColumnCount > MAX_GRID_COLUMNS || nextColumnCount * rows.length > MAX_GRID_CELLS) throw new Error('table-size');
        columnCount = nextColumnCount;
        for (let offsetRow = 0; offsetRow < rowSpan; offsetRow++)
          for (let offsetColumn = 0; offsetColumn < columnSpan; offsetColumn++) {
            if (grid[rowIndex + offsetRow][columnIndex + offsetColumn] !== undefined) throw new Error('table-overlap');
            grid[rowIndex + offsetRow][columnIndex + offsetColumn] = '';
          }
        grid[rowIndex][columnIndex] = format === 'markdown' ? cellMarkdown(cell) : cellText(cell);
        columnIndex += columnSpan;
      }
    }
    return { rows, grid: grid.map(row => Array.from({ length: columnCount }, (_, index) => row[index] ?? '')), columnCount };
  }
  function tableSource(table, format) {
    const { rows, grid, columnCount } = rectangularCells(table, format);
    if (!columnCount) return null;
    if (format === 'tsv') {
      const field = text => /[\t\r\n"]/.test(text) ? '"' + text.replace(/"/g, '""') + '"' : text;
      return grid.map(row => row.map(field).join('\t')).join('\r\n');
    }
    const hasHeader = [...rows[0].cells].some(cell => cell.tagName === 'TH');
    const header = hasHeader ? grid.shift() : Array(columnCount).fill('');
    const line = row => '| ' + row.join(' | ') + ' |';
    return [line(header), line(Array(columnCount).fill('---')), ...grid.map(line)].join('\n');
  }
  function requestCopy(state, format) {
    if (!state.block.isConnected || !hasCells(state.table)) return;
    setMenuOpen(state, false); cancelPending(state); resetFeedback(state);
    if (typeof hooks.requestCopy !== 'function') { showFeedback(state, false); return; }
    let source;
    try { source = tableSource(state.table, format); }
    catch { showFeedback(state, false); return; }
    if (source === null) return;
    const requestId = 'table-' + requestNonce + '-' + (++copySequence);
    while (pendingCopies.size >= MAX_PENDING_COPIES) {
      const firstId = pendingCopies.keys().next().value, previous = pendingCopies.get(firstId);
      pendingCopies.delete(firstId);
      if (previous.copyRequestId === firstId) previous.copyRequestId = null;
    }
    state.copyRequestId = requestId; pendingCopies.set(requestId, state);
    try {
      if (hooks.requestCopy(source, state.block, requestId) === false && state.copyRequestId === requestId) {
        cancelPending(state); showFeedback(state, false);
      }
    }
    catch {
      if (state.copyRequestId === requestId && state.block.isConnected) { cancelPending(state); showFeedback(state, false); }
    }
  }
  function acknowledgeCopy(command) {
    const state = pendingCopies.get(command?.requestId);
    pendingCopies.delete(command?.requestId);
    if (!state || !state.block.isConnected || state.copyRequestId !== command.requestId) return;
    state.copyRequestId = null;
    // Native acknowledgement alone confirms the clipboard write; stale requests cannot update another view.
    // 只有原生回执确认剪贴板写入成功，过期请求不能更新另一个界面。
    showFeedback(state, command.success === true);
  }
  function resetCopies() {
    pendingCopies.clear();
    for (const state of records) { state.copyRequestId = null; setMenuOpen(state, false); resetFeedback(state); }
  }
  function disposeDetached() {
    for (const state of records) {
      if (state.block.isConnected) continue;
      cancelPending(state); setMenuOpen(state, false); resetFeedback(state); records.delete(state);
    }
  }
  function setLanguage(next) {
    Object.assign(strings, next?.strings || next || {});
    for (const state of records) if (state.block.isConnected) localize(state);
  }
  document.addEventListener('pointerdown', event => {
    if (openMenuState && !openMenuState.header.contains(event.target)) setMenuOpen(openMenuState, false);
  });
  document.addEventListener('focusin', event => {
    if (openMenuState && !openMenuState.header.contains(event.target)) setMenuOpen(openMenuState, false);
  });
  document.addEventListener('keydown', event => {
    if (event.key !== 'Escape' || event.isComposing || event.keyCode === 229 || !openMenuState) return;
    event.preventDefault(); dismissMenu();
  });
  window.kynxaTables = { configure, hydrate, setLanguage, acknowledgeCopy, resetCopies, disposeDetached, dismissMenu };
})();
