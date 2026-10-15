/* Literal conversation search paints ranges without changing message or clipboard DOM.
 * 会话正文按字面查找，仅绘制范围，不改动消息节点或剪贴板内容。 */
(() => {
  'use strict';
  const REFRESH_DELAY_MS = 160;
  const WORK_BUDGET_MS = 8;
  const EXCLUDED = '[data-copy-ignore],[hidden],svg,script,style,.katex-mathml,annotation';
  const BOUNDARIES = 'pre,p,li,td,th,h1,h2,h3,h4,h5,h6,blockquote';
  const strings = {
    findText: '查找会话正文', findTextPlaceholder: '查找当前会话…', findTextPrevious: '上一处',
    findTextNext: '下一处', findTextClose: '关闭正文查找', findTextNoResults: '没有找到匹配内容',
    findTextCount: '{0} / {1}', findTextHint: 'Enter / ↓ 下一处 · Shift+Enter / ↑ 上一处 · F3 / Shift+F3 切换 · Esc 关闭',
    findTextSelection: '请先取消文字选择，再跳转到匹配内容。', findTextSearching: '正在查找…'
  };
  const bar = document.getElementById('transcript-search');
  const input = document.getElementById('transcript-search-input');
  const count = document.getElementById('transcript-search-count');
  const status = document.getElementById('transcript-search-status');
  const previous = bar.querySelector('[data-search-action="previous"]');
  const next = bar.querySelector('[data-search-action="next"]');
  const close = bar.querySelector('[data-search-action="close"]');
  let hooks = {}, conversationId = null, open = false, query = '', matches = [], index = -1;
  let topInsetPx = 0;
  let searching = false, refreshTimer = 0, workTimer = 0, generation = 0, blocked = false, composing = false;
  let scanning = false, needsRefresh = false;
  let refreshDueAtMs = 0, lastStartedAtMs = 0;
  let requestedMatch = null;
  const canHighlight = typeof Highlight === 'function' && !!window.CSS?.highlights;

  function configure(options) { hooks = { ...hooks, ...options }; }
  function translated(key) { return strings[key] || key; }
  function matchKey(match) {
    return match ? { messageId: match.messageId, group: match.group, offset: match.offset, length: match.length } : null;
  }
  function sameMatch(match, key) {
    return !!key && match.messageId === key.messageId && match.group === key.group && match.offset === key.offset && match.length === key.length;
  }
  function updateControls() {
    bar.hidden = !open;
    bar.setAttribute('aria-label', translated('findText'));
    input.placeholder = translated('findTextPlaceholder'); input.setAttribute('aria-label', translated('findText'));
    for (const [button, key] of [[previous, 'findTextPrevious'], [next, 'findTextNext'], [close, 'findTextClose']]) {
      button.setAttribute('aria-label', translated(key));
      // Keep keyboard hints in the bar instead of creating another floating native tooltip.
      // 键盘提示留在搜索栏内，避免增加悬空的原生提示气泡。
      button.removeAttribute('title');
    }
    previous.disabled = next.disabled = !matches.length;
    const totals = translated('findTextCount').replace('{0}', String(index + 1)).replace('{1}', String(matches.length));
    count.textContent = searching ? matches.length ? totals + ' · …' : '…' : totals;
    status.textContent = blocked ? translated('findTextSelection') : searching ? translated('findTextSearching')
      : query && !matches.length ? translated('findTextNoResults') : translated('findTextHint');
    status.dataset.searchState = blocked ? 'selection' : searching ? 'searching' : query && !matches.length ? 'empty' : 'ready';
  }
  function clearHighlights() {
    if (!canHighlight) return;
    CSS.highlights.delete('transcript-search-all'); CSS.highlights.delete('transcript-search-current');
  }
  function paintHighlights() {
    clearHighlights();
    if (!open || !canHighlight) return;
    const all = new Highlight();
    for (const match of matches) all.add(match.range);
    all.priority = 0; CSS.highlights.set('transcript-search-all', all);
    if (index >= 0 && matches[index]) {
      const active = new Highlight(matches[index].range); active.priority = 1;
      CSS.highlights.set('transcript-search-current', active);
    }
  }
  function cancelWork() {
    generation++; clearTimeout(refreshTimer); refreshTimer = 0; clearTimeout(workTimer); workTimer = 0;
    searching = false; scanning = false; needsRefresh = false;
  }
  function setTopInset(insetPx, preserveReading = false) {
    const nextInsetPx = Math.max(0, Math.ceil(insetPx)), deltaPx = nextInsetPx - topInsetPx;
    if (!deltaPx) return;
    topInsetPx = nextInsetPx;
    document.getElementById('messages').style.paddingTop = topInsetPx ? topInsetPx + 'px' : '';
    if (preserveReading) hooks.navigate?.(Math.max(0, scrollY + deltaPx));
  }
  function capture() { return { open, query, active: matchKey(matches[index]) || requestedMatch, topInsetPx }; }
  function attachConversation(id, saved = null) {
    cancelWork(); clearHighlights(); matches = []; index = -1; blocked = false;
    conversationId = id; open = saved?.open === true; query = typeof saved?.query === 'string' ? saved.query : '';
    requestedMatch = saved?.active || null; input.value = query;
    refreshDueAtMs = 0; lastStartedAtMs = 0; updateControls(); hooks.stateChanged?.(open);
    setTopInset(open && Number.isFinite(saved?.topInsetPx) ? Math.min(saved.topInsetPx, bar.getBoundingClientRect().height + 24) : 0);
    if (open && query) refresh(true);
  }
  function show() {
    if (document.querySelector('.mermaid-viewer[open]')) { hooks.stateChanged?.(open); return false; }
    const wasOpen = open;
    open = true; blocked = false; hooks.stopFollowing?.(); updateControls();
    if (!wasOpen) hooks.stateChanged?.(true);
    input.focus({ preventScroll: true }); input.select();
    refresh(true);
    return true;
  }
  function hide(notify = true) {
    if (!open) return;
    requestedMatch = matchKey(matches[index]) || requestedMatch;
    open = false; cancelWork(); clearHighlights(); updateControls();
    setTopInset(0, true);
    hooks.stateChanged?.(false);
    if (notify) hooks.closed?.();
  }
  function setLanguage(nextStrings) { Object.assign(strings, nextStrings || {}); updateControls(); }

  function createGroup(body, groupNumber, boundary) {
    return { body, boundary, number: groupNumber, messageId: body.closest('article[data-message-id]')?.dataset.messageId || '', text: '', pieces: [] };
  }
  function appendPiece(group, node, text) {
    group.pieces.push({ node, start: group.text.length, end: group.text.length + text.length }); group.text += text;
  }
  function acceptSearchNode(node) {
    const element = node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement;
    if (element?.closest(EXCLUDED)) return NodeFilter.FILTER_REJECT;
    if (node.nodeType === Node.TEXT_NODE || node.nodeName === 'BR') return NodeFilter.FILTER_ACCEPT;
    return NodeFilter.FILTER_SKIP;
  }
  function rangeEndpoint(piece, offset) {
    if (piece.node.nodeType === Node.TEXT_NODE) return [piece.node, Math.max(0, Math.min(piece.node.length, offset - piece.start))];
    const parent = piece.node.parentNode, position = Array.prototype.indexOf.call(parent.childNodes, piece.node);
    return [parent, position + (offset > piece.start ? 1 : 0)];
  }
  function createRange(group, start, end, pieceIndex) {
    while (pieceIndex < group.pieces.length - 1 && group.pieces[pieceIndex].end <= start) pieceIndex++;
    let endIndex = pieceIndex;
    while (endIndex < group.pieces.length - 1 && group.pieces[endIndex].end < end) endIndex++;
    const range = document.createRange(), first = rangeEndpoint(group.pieces[pieceIndex], start), last = rangeEndpoint(group.pieces[endIndex], end);
    range.setStart(...first); range.setEnd(...last);
    return { range, pieceIndex };
  }
  function eligibleElement(element, body, cache) {
    const visited = [];
    while (element !== body && element && !cache.has(element)) {
      visited.push(element);
      if (element.matches(EXCLUDED)) { for (const node of visited) cache.set(node, false); return false; }
      element = element.parentElement;
    }
    const valid = element === body || !!element && cache.get(element) === true;
    for (const node of visited) cache.set(node, valid);
    return valid;
  }
  function currentMatch(match, validation = { bodies: new Map(), elements: new WeakMap() }) {
    const range = match.range;
    let owner = validation.bodies.get(match.body);
    if (owner === undefined) {
      owner = match.body.isConnected && !match.body.closest(EXCLUDED)
        ? match.body.closest('#messages article[data-message-id]')?.dataset.messageId ?? null : null;
      validation.bodies.set(match.body, owner);
    }
    if (!range.startContainer.isConnected || !range.endContainer.isConnected
        || owner !== match.messageId || !match.body.contains(range.startContainer) || !match.body.contains(range.endContainer)) return false;
    const firstElement = range.startContainer.nodeType === Node.TEXT_NODE ? range.startContainer.parentElement : range.startContainer;
    const lastElement = range.endContainer.nodeType === Node.TEXT_NODE ? range.endContainer.parentElement : range.endContainer;
    // Only semantic hidden controls/bodies are excluded. Clipped code and closed source details remain searchable.
    // 只排除语义上隐藏的控件和正文；裁切代码及关闭的源码详情仍可搜索。
    if (!eligibleElement(firstElement, match.body, validation.elements) || !eligibleElement(lastElement, match.body, validation.elements)) return false;
    const selectedText = range.startContainer === range.endContainer && range.startContainer.nodeType === Node.TEXT_NODE
      ? range.startContainer.data.slice(range.startOffset, range.endOffset) : range.toString();
    if (selectedText === match.text) return true;
    // Range text includes hidden math branches and omits BR. Use the exact index rules on an inert clone.
    // Range 文本包含隐藏数学分支且省略 BR；离线克隆必须复用索引规则，以验证相同的逻辑正文。
    const fragment = range.cloneContents(), walker = document.createTreeWalker(fragment, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT,
      { acceptNode: acceptSearchNode });
    let text = '', node;
    while ((node = walker.nextNode())) {
      if (node.nodeType === Node.TEXT_NODE) text += node.textContent;
      else if (node.nodeName === 'BR') text += '\n';
    }
    return text === match.text;
  }
  function retainPublished() {
    if (!matches.length) return;
    const active = matchKey(matches[index]) || requestedMatch, validation = { bodies: new Map(), elements: new WeakMap() };
    const valid = matches.filter(match => currentMatch(match, validation));
    if (valid.length === matches.length) return;
    matches = valid; index = active ? matches.findIndex(match => sameMatch(match, active)) : -1;
    requestedMatch = active; paintHighlights();
  }
  function refresh(immediate = false, force = false, verified = false) {
    if (!open) return;
    // Reuse one validation cache per refresh; unchanged prose keeps its painted ranges during a background scan.
    // 每次刷新复用一份校验缓存，后台扫描期间保留未变化正文的高亮，并移除已经失效的范围。
    if (!immediate && !force && !verified) retainPublished();
    // A render only marks the in-flight scan dirty. Query/navigation changes still cancel immediately.
    // 渲染只把进行中的扫描标为待刷新；关键词或会话导航变化仍立即取消旧扫描。
    if (!immediate && !force && searching) {
      if (scanning) needsRefresh = true;
      updateControls(); return;
    }
    const nowMs = performance.now(), dueAtMs = immediate ? nowMs : refreshTimer ? refreshDueAtMs
      : Math.min(nowMs + REFRESH_DELAY_MS, lastStartedAtMs ? lastStartedAtMs + 240 : nowMs + REFRESH_DELAY_MS);
    requestedMatch = matchKey(matches[index]) || requestedMatch;
    cancelWork(); blocked = false;
    if (immediate || force || !query) { clearHighlights(); matches = []; index = -1; }
    if (!query) { updateControls(); return; }
    searching = true; updateControls();
    const expectedGeneration = generation;
    const startSearch = () => {
      refreshTimer = 0; refreshDueAtMs = 0; lastStartedAtMs = performance.now();
      if (expectedGeneration !== generation || !open) return;
      scanning = true;
      const bodies = [...document.querySelectorAll('#messages .message-body')].filter(body => !body.closest('[hidden]'));
      let bodyIndex = 0, bodyPosition = 0, walker = null, group = null, groupNumber = 0, searchGroup = null, expression = null, pieceIndex = 0;
      const results = [];
      // Unicode ignore-case matching leaves offsets in the original string, including length-changing case folds.
      // Unicode 忽略大小写匹配直接返回原字符串偏移，不依靠可能改变字符长度的大小写转换。
      const pattern = query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const beginGroupSearch = current => {
        if (!current?.text) return;
        searchGroup = current; expression = new RegExp(pattern, 'giu'); pieceIndex = 0;
      };
      const tick = () => {
        workTimer = 0;
        if (expectedGeneration !== generation || !open) return;
        const deadlineMs = performance.now() + WORK_BUDGET_MS;
        do {
          if (searchGroup) {
            const match = expression.exec(searchGroup.text);
            if (match) {
              try {
                const created = createRange(searchGroup, match.index, match.index + match[0].length, pieceIndex); pieceIndex = created.pieceIndex;
                results.push({ range: created.range, body: searchGroup.body, messageId: searchGroup.messageId, group: searchGroup.number,
                  offset: match.index, length: match[0].length, text: match[0] });
              } catch {
                // Detached BR parents or shortened nodes can invalidate an endpoint during a render.
                // 渲染时脱离的 BR 父节点或缩短的文本可能使端点失效，由下一轮扫描恢复。
                needsRefresh = true;
              }
              continue;
            }
            searchGroup = null;
          }
          if (!walker) {
            if (bodyIndex >= bodies.length) {
              // A stream may replace nodes between work slices. Publish only still-valid literal ranges.
              // 流式输出可能在分片之间替换节点；仅发布仍连接且字面内容一致的范围。
              const validation = { bodies: new Map(), elements: new WeakMap() };
              matches = results.filter(match => currentMatch(match, validation));
              const refreshAgain = needsRefresh || matches.length < results.length;
              searching = false; scanning = false; needsRefresh = false;
              index = requestedMatch ? matches.findIndex(match => sameMatch(match, requestedMatch)) : -1;
              requestedMatch = matchKey(matches[index]); paintHighlights(); updateControls();
              if (refreshAgain) refresh(false, false, true);
              return;
            }
            const body = bodies[bodyIndex++]; groupNumber = 0;
            const article = body.closest('#messages article[data-message-id]');
            if (!body.isConnected || body.closest('[hidden]') || !article) { needsRefresh = true; continue; }
            bodyPosition = [...article.querySelectorAll('.message-body')].indexOf(body);
            walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, { acceptNode: acceptSearchNode });
          }
          if (!walker.root.isConnected || walker.root.closest('[hidden]')) { walker = null; group = null; continue; }
          const node = walker.nextNode();
          if (!node) { walker = null; beginGroupSearch(group); group = null; continue; }
          const parent = node.parentElement;
          const boundary = parent.closest(BOUNDARIES) || walker.root;
          if (!group || group.boundary !== boundary) {
            const prior = group; group = createGroup(walker.root, bodyPosition + ':' + groupNumber++, boundary);
            appendPiece(group, node, node.nodeName === 'BR' ? '\n' : node.textContent);
            beginGroupSearch(prior);
          } else appendPiece(group, node, node.nodeName === 'BR' ? '\n' : node.textContent);
        } while (performance.now() < deadlineMs);
        workTimer = setTimeout(tick, 0);
      };
      tick();
    };
    // Preserve an existing deadline so a continuous stream cannot postpone search indefinitely.
    // 保留已有刷新期限，避免持续流式输出把查找一直推迟到回复结束。
    refreshDueAtMs = dueAtMs; refreshTimer = setTimeout(startSearch, Math.max(0, dueAtMs - performance.now()));
  }
  function navigate(direction) {
    if (!open || !matches.length || document.querySelector('.mermaid-viewer[open]')) return false;
    if (hooks.canNavigate?.() === false) { blocked = true; updateControls(); return false; }
    blocked = false;
    const nextIndex = index < 0 ? direction < 0 ? matches.length - 1 : 0 : (index + direction + matches.length) % matches.length;
    const match = matches[nextIndex];
    if (!currentMatch(match)) { refresh(true); return false; }
    index = nextIndex;
    const element = match.range.startContainer.nodeType === Node.TEXT_NODE
      ? match.range.startContainer.parentElement : match.range.startContainer;
    if (!element.isConnected) { refresh(true); return false; }
    hooks.stopFollowing?.();
    const block = element.closest('.content-block');
    if (block && window.kynxaContentBlocks?.reveal(block) === false) { blocked = true; updateControls(); return false; }
    const diagram = element.closest('.mermaid-block');
    if (diagram) window.kynxaDiagrams?.revealSource(diagram);
    for (let ancestor = element; ancestor && ancestor.id !== 'messages'; ancestor = ancestor.parentElement)
      if (ancestor.tagName === 'DETAILS') ancestor.open = true;
    // Reveal wide code/table matches inside their own scrollers before moving the page vertically.
    // 先在代码或表格自己的滚动容器内展示横向匹配，再移动页面纵向位置。
    for (let ancestor = element; ancestor && ancestor.id !== 'messages'; ancestor = ancestor.parentElement) {
      if (!ancestor.matches('pre,.table-scroll')) continue;
      const rect = match.range.getBoundingClientRect(), bounds = ancestor.getBoundingClientRect();
      if (rect.left < bounds.left + 12 || rect.right > bounds.right - 12)
        ancestor.scrollLeft += rect.left - bounds.left - Math.min(60, bounds.width / 4);
    }
    let rect = match.range.getBoundingClientRect(), documentTop = scrollY + rect.top;
    let maximumScroll = Math.max(0, document.documentElement.scrollHeight - innerHeight);
    const barBounds = bar.getBoundingClientRect(), safeTop = barBounds.bottom + 16;
    let top = Math.min(maximumScroll, Math.max(0, documentTop - Math.max(safeTop, Math.min(innerHeight * .35, innerHeight - 80))));
    const predictedTop = documentTop - top;
    // Keep the find bar anchored at the top. Only explicit navigation to an occluded first-screen match needs a small inset.
    // 搜索栏固定在顶部；仅在主动定位被遮挡的首屏匹配时补上必要的顶部避让，不随匹配跳到右下角。
    if (predictedTop < barBounds.bottom + 8 && predictedTop + rect.height > barBounds.top - 8
        && rect.left < barBounds.right + 8 && rect.right > barBounds.left - 8) {
      setTopInset(topInsetPx + Math.max(0, safeTop - predictedTop));
      rect = match.range.getBoundingClientRect(); documentTop = scrollY + rect.top;
      maximumScroll = Math.max(0, document.documentElement.scrollHeight - innerHeight);
      top = Math.min(maximumScroll, Math.max(0, documentTop - safeTop));
    }
    hooks.navigate?.(top);
    requestedMatch = matchKey(match); paintHighlights(); updateControls(); return true;
  }
  input.addEventListener('input', () => { query = input.value; requestedMatch = null; index = -1; refresh(false, true); });
  input.addEventListener('compositionstart', () => { composing = true; });
  input.addEventListener('compositionend', () => { composing = false; });
  // A blur may end/cancel native composition without another key reaching this field.
  // 失焦可能由原生输入法结束或取消组词，清理局部状态；按键自身的 isComposing 仍阻止误导航。
  input.addEventListener('blur', () => { composing = false; });
  for (const button of [previous, next, close]) button.addEventListener('pointerdown', event => {
    if (event.pointerType === 'mouse' && event.button === 0) event.preventDefault();
  });
  previous.addEventListener('click', () => navigate(-1)); next.addEventListener('click', () => navigate(1)); close.addEventListener('click', () => hide());
  window.addEventListener('keydown', event => {
    if (composing || event.isComposing || event.keyCode === 229) return;
    const key = event.key.toLowerCase();
    if ((event.ctrlKey || event.metaKey) && !event.altKey &&
        (key === 'f' && !event.shiftKey || key === 'k' && event.shiftKey)) {
      event.preventDefault(); event.stopImmediatePropagation();
      if (document.querySelector('.mermaid-viewer[open]')) return;
      if (event.shiftKey) hooks.searchConversations?.(); else show();
    } else if (open && event.key === 'Escape' && !document.querySelector('.mermaid-viewer[open]')) {
      event.preventDefault(); event.stopImmediatePropagation();
      // Dismiss the foreground menu before the search surface underneath it.
      // 优先关闭搜索栏上方的表格菜单，下一次 Escape 才关闭搜索。
      if (!window.kynxaTables?.dismissMenu()) hide();
    } else if (open && !document.querySelector('.mermaid-viewer[open]') && !event.ctrlKey && !event.metaKey && !event.altKey
        && (event.key === 'F3' || event.target === input && ['Enter', 'ArrowUp', 'ArrowDown'].includes(event.key))) {
      event.preventDefault(); event.stopImmediatePropagation();
      navigate(event.key === 'ArrowUp' || event.shiftKey ? -1 : 1);
    }
  }, true);
  window.addEventListener('pagehide', () => { cancelWork(); clearHighlights(); matches = []; });
  window.kynxaTranscriptSearch = { configure, show, hide, refresh, capture, attachConversation, setLanguage, navigate,
    state: () => ({ open, query, count: matches.length, index, conversationId, searching, canHighlight, position: 'top', topInsetPx }) };
  updateControls();
})();
