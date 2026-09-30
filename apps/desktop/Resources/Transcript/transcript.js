/* One browser selection surface for the whole conversation. KaTeX stays real DOM. */
(() => {
  'use strict';
  const messages = document.getElementById('messages');
  const entries = new Map();
  const mathCache = new Map();
  let conversationId = null, pending = null, following = true, applying = false;
  let nextMathId = 0, mathCacheBytes = 0, scrollFrame = 0, flushFrame = 0;
  let pointerSelecting = false;
  const send = value => window.chrome?.webview?.postMessage(value);
  const atBottom = () => document.documentElement.scrollHeight - innerHeight - scrollY <= 36;
  const activeSelection = () => {
    const selection = getSelection();
    if (!selection || selection.isCollapsed || !selection.rangeCount) return false;
    try { return selection.getRangeAt(0).intersectsNode(messages); } catch { return false; }
  };
  function followBottom() {
    if (!following || activeSelection() || pointerSelecting) return;
    cancelAnimationFrame(scrollFrame);
    scrollFrame = requestAnimationFrame(() => {
      if (following && !activeSelection() && !pointerSelecting) scrollTo({ top: document.documentElement.scrollHeight, behavior: 'instant' });
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
    const reasoning = document.createElement('details'); reasoning.className = 'reasoning';
    const summary = document.createElement('summary'); summary.dataset.copyIgnore = '';
    const thought = document.createElement('div'); thought.className = 'reasoning-body';
    reasoning.append(summary, thought); reasoning.open = !!message.reasoningExpanded;
    reasoning.addEventListener('toggle', followBottom);
    const body = document.createElement('div'); body.className = 'message-body';
    const status = document.createElement('div'); status.className = 'message-status'; status.dataset.copyIgnore = '';
    const actions = document.createElement('div'); actions.className = 'message-actions'; actions.dataset.copyIgnore = '';
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'copy-message';
    copy.title = '复制'; copy.setAttribute('aria-label', '复制整条消息');
    copy.innerHTML = '<svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
    const retry = document.createElement('button'); retry.type = 'button'; retry.className = 'retry'; retry.textContent = '重试';
    const entry = { article, content, reasoning, summary, thought, body, status, actions, copy, retry, message };
    copy.addEventListener('click', () => send({ type: 'copy', conversationId, text: entry.message.content || '' }));
    retry.addEventListener('click', () => send({ type: 'retry', conversationId, id: entry.message.id }));
    actions.append(copy, retry); content.append(reasoning, body, status); article.append(content, actions);
    return entry;
  }
  function updateMessage(entry, message) {
    entry.message = message;
    const role = message.role === 'user' ? 'user' : 'assistant';
    entry.article.className = 'message ' + role;
    entry.article.dataset.role = role;
    if (role === 'assistant' && !entry.avatar) {
      entry.avatar = document.createElement('img'); entry.avatar.className = 'message-avatar';
      entry.avatar.src = '../UI/Brand/kynxa-logo.png'; entry.avatar.alt = ''; entry.avatar.draggable = false; entry.avatar.dataset.copyIgnore = '';
      entry.article.prepend(entry.avatar);
    } else if (role === 'user' && entry.avatar) { entry.avatar.remove(); entry.avatar = null; }
    entry.reasoning.hidden = !message.reasoningHtml && !message.reasoningTitle;
    entry.summary.textContent = message.reasoningTitle || '思考过程';
    patchBlocks(entry.thought, message.reasoningHtml || '');
    if (role === 'user') {
      const text = message.content || '';
      if (entry.body._plainContent !== text || !entry.body.hasAttribute('data-copy-plain')) {
        entry.body.textContent = text;
        entry.body.dataset.copyPlain = '';
        entry.body._plainContent = text;
        delete entry.body._sourceHtml; delete entry.body._blockKeys;
      }
    } else {
      if (entry.body.hasAttribute('data-copy-plain')) {
        entry.body.removeAttribute('data-copy-plain'); entry.body.replaceChildren();
        delete entry.body._sourceHtml; delete entry.body._blockKeys;
      }
      patchBlocks(entry.body, message.html || '');
    }
    entry.status.replaceChildren();
    if (message.error) entry.status.textContent = typeof message.error === 'string' ? message.error : '回复中断，请重试。';
    else if (message.waiting && !message.content) entry.status.textContent = '正在回复…';
    if (message.streaming) { const dot = document.createElement('span'); dot.className = 'streaming-dot'; dot.setAttribute('aria-label', '正在生成'); entry.status.append(dot); }
    entry.retry.hidden = !message.canRetry;
    entry.copy.hidden = !message.content;
  }
  function applyTranscript(snapshot) {
    if (!snapshot || !Array.isArray(snapshot.messages)) return;
    const changedConversation = conversationId !== snapshot.conversationId;
    if (changedConversation || snapshot.openAtBottom) {
      pending = null; clearSelection(); following = true;
      if (changedConversation) { entries.clear(); messages.replaceChildren(); }
      conversationId = snapshot.conversationId;
    } else if (activeSelection()) { pending = snapshot; return; }
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
    applying = false;
    followBottom();
    document.fonts.ready.then(followBottom);
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
    if (!applying) following = !activeSelection() && !pointerSelecting && atBottom();
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
    if (command?.type === 'render') applyTranscript(command);
    else if (command?.type === 'clearSelection') clearSelection();
    else if (command?.type === 'beforeSend') { following = atBottom() && !activeSelection(); followBottom(); }
  });
  window.applyTranscript = applyTranscript;
  window.transcriptSelectionText = selectionText;
  window.transcriptState = () => ({ conversationId, pending: !!pending, following, selection: activeSelection(), messageCount: entries.size });
  send({ type: 'ready' });
})();
