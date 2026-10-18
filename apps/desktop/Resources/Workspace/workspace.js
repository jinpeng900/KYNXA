/* This page renders gateway-authorized content only; document text cannot send native actions.
 * 此页只展示网关已授权内容，文档文本不能执行原生动作。 */
(() => {
  'use strict';
  const content = document.getElementById('content');
  const states = new Map();
  let key = '', revision = 0, scrollTimer = 0;
  const post = message => window.chrome?.webview?.postMessage(message);

  function saveState() {
    if (!key) return;
    const state = { x: scrollX, y: scrollY };
    states.delete(key); states.set(key, state);
    while (states.size > 24) states.delete(states.keys().next().value);
    post({ type: 'state', key, state });
  }

  function readState(value) {
    if (!value) return null;
    try { return typeof value === 'string' ? JSON.parse(value) : value; } catch { return null; }
  }

  function restoreState(state) {
    if (Number.isFinite(state?.x) && Number.isFinite(state?.y)) scrollTo(state.x, state.y);
    else scrollTo(0, 0);
  }

  async function show(message) {
    saveState();
    key = message.key || '';
    const current = ++revision;
    content.replaceChildren();
    window.kynxaDiagrams?.disposeDetached();
    if (message.kind === 'markdown' || message.kind === 'mermaid' || message.kind === 'code') {
      // HTML is produced by the existing HTML-disabled Markdown renderer, never passed through from a document.
      // HTML 仅来自现有禁止原生 HTML 的 Markdown 渲染器，绝不直接接受文档内的 HTML。
      content.innerHTML = typeof message.html === 'string' ? message.html : '';
      for (const formula of content.querySelectorAll('.math[data-latex]')) {
        if (!window.katex) continue;
        try { window.katex.render(formula.dataset.latex || '', formula,
          { displayMode: formula.dataset.display === 'true', throwOnError: false, trust: false, strict: 'ignore' }); } catch { }
      }
      window.kynxaDiagrams?.hydrate(content);
    } else if (message.kind === 'image' && typeof message.contentUrl === 'string') {
      const image = document.createElement('img'); image.alt = message.title || '';
      const wrapper = document.createElement('div'); wrapper.className = 'image-view'; wrapper.append(image); content.append(wrapper);
      image.addEventListener('click', () => image.classList.toggle('actual-size'));
      image.addEventListener('load', () => { if (revision === current) restoreState(states.get(key) || readState(message.state)); });
      image.src = message.contentUrl;
    } else {
      const text = document.createElement('pre'); text.className = 'plain-text';
      text.textContent = typeof message.text === 'string' ? message.text : ''; content.append(text);
    }
    document.title = message.title || '文件预览';
    requestAnimationFrame(() => { if (revision === current) restoreState(states.get(key) || readState(message.state)); });
    // A hidden tab finishes its own images and diagrams before the native host suspends it.
    // 后台标签先完成自己的图片和图表，原生宿主再挂起该视图。
    await contentSettled();
    if (revision === current) post({ type: 'shown', key, renderId: message.renderId });
  }

  function contentSettled() {
    const images = [...content.querySelectorAll('img')].filter(image => !image.complete);
    const diagrams = [...content.querySelectorAll('.mermaid-block[data-mermaid-ready="true"]')];
    const pending = () => images.some(image => !image.complete) || diagrams.some(block =>
      !['ready', 'error'].includes(block.dataset.diagramState));
    if (!pending()) return Promise.resolve();
    return new Promise(resolve => {
      let timeout;
      const observer = new MutationObserver(check);
      function done() { clearTimeout(timeout); observer.disconnect(); resolve(); }
      function check() { if (!pending()) done(); }
      observer.observe(content, { attributes: true, subtree: true, childList: true });
      for (const image of images) {
        image.addEventListener('load', check, { once: true });
        image.addEventListener('error', check, { once: true });
      }
      timeout = setTimeout(done, 40000);
      check();
    });
  }

  function appearance(palette) {
    for (const [name, value] of Object.entries(palette || {})) {
      if (/^[a-z]+$/.test(name) && /^#[0-9a-f]{6}$/i.test(value))
        document.documentElement.style.setProperty('--appearance-' + name, value);
    }
  }

  window.kynxaDiagrams?.configure({ requestCopy: (text, block, requestId) => {
    post({ type: 'copy', key, text }); window.kynxaDiagrams.acknowledgeCopy({ requestId, ok: true });
  } });
  window.chrome?.webview?.addEventListener('message', event => {
    const message = event.data;
    if (message?.type === 'show') show(message);
    else if (message?.type === 'saveState') saveState();
    else if (message?.type === 'appearance') appearance(message.palette);
  });
  document.addEventListener('click', event => {
    const link = event.target.closest('a[href]');
    if (!link) return;
    event.preventDefault();
    try { const url = new URL(link.getAttribute('href')); if (/^https?:$/.test(url.protocol)) post({ type: 'link', key, url: url.href }); } catch { }
  });
  window.addEventListener('scroll', () => { clearTimeout(scrollTimer); scrollTimer = setTimeout(saveState, 120); }, { passive: true });
  post({ type: 'ready' });
})();
