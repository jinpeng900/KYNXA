/* This page renders gateway-authorized content only; document text cannot send native actions.
 * 此页只展示网关已授权内容，文档文本不能执行原生动作。 */
(() => {
  'use strict';
  const content = document.getElementById('content');
  const states = new Map();
  let key = '', revision = 0, scrollTimer = 0, docxView = null, officeView = null, documentRead = null;
  const post = message => window.chrome?.webview?.postMessage(message);

  function saveState() {
    if (!key) return;
    const state = { ...(officeView?.getState?.() || {}), x: scrollX, y: scrollY };
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
    documentRead?.abort(); documentRead = null;
    officeView?.dispose?.(); officeView = null;
    releaseDocx(docxView); docxView = null;
    content.replaceChildren();
    document.body.dataset.previewKind = message.kind || '';
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
    } else if (['docx', 'pptx', 'xlsx'].includes(message.kind)) {
      try {
        if (!await renderOffice(message, current)) return;
      } catch {
        if (revision === current) post({ type: message.kind + '-failed', key: message.key || '', renderId: message.renderId });
        return;
      }
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
    // Office modules report their usable first view independently from background resource completion.
    // Office 模块将可操作首屏与后台资源完成分开回报，不让字体、图片及后续页阻塞 shown。
    if (!['docx', 'pptx', 'xlsx'].includes(message.kind)) await contentSettled();
    if (revision === current) post({ type: 'shown', key, renderId: message.renderId });
  }

  function contentSettled(root = content, options = {}) {
    const images = [...root.querySelectorAll('img')].filter(image => {
      if (image.complete) return false;
      if (!options.visibleOnly) return true;
      const rect = image.getBoundingClientRect(), viewport = image.ownerDocument.defaultView;
      return rect.bottom >= 0 && rect.top <= viewport.innerHeight && rect.right >= 0 && rect.left <= viewport.innerWidth;
    });
    const diagrams = [...root.querySelectorAll('.mermaid-block[data-mermaid-ready="true"]')];
    const pending = () => images.some(image => !image.complete) || diagrams.some(block =>
      !['ready', 'error'].includes(block.dataset.diagramState));
    if (!pending()) return Promise.resolve();
    return new Promise(resolve => {
      let timeout;
      const observer = new MutationObserver(check);
      function done() { clearTimeout(timeout); observer.disconnect(); options.signal?.removeEventListener('abort', done); resolve(); }
      function check() { if (!pending()) done(); }
      observer.observe(root, { attributes: true, subtree: true, childList: true });
      for (const image of images) {
        image.addEventListener('load', check, { once: true });
        image.addEventListener('error', check, { once: true });
      }
      options.signal?.addEventListener('abort', done, { once: true });
      timeout = setTimeout(done, 40000);
      if (options.signal?.aborted) done(); else check();
    });
  }

  function releaseDocx(view) {
    if (!view || view.disposed) return;
    view.disposed = true;
    view.observer?.disconnect();
    clearTimeout(view.scrollTimer);
    for (const cleanup of view.cleanups || []) cleanup();
    for (const url of view.urls) URL.revokeObjectURL(url);
    view.urls.clear();
  }

  function cleanDocxDocument(root, urls) {
    root.querySelectorAll('script,iframe,object,embed,link,meta,base,form,input,button,select,textarea').forEach(node => node.remove());
    for (const node of root.querySelectorAll('*')) {
      for (const attribute of [...node.attributes]) {
        const name = attribute.localName.toLowerCase(), value = attribute.value.trim();
        if (name.startsWith('on') || name === 'srcdoc') node.removeAttributeNode(attribute);
        else if (name === 'src' || name === 'href' || name === 'poster') {
          const isLink = node.localName === 'a' && name === 'href';
          const allowed = isLink ? /^(#|https?:\/\/)/i.test(value) : /^(blob:|data:image\/)/i.test(value);
          if (!allowed) node.removeAttributeNode(attribute);
          else if (value.startsWith('blob:')) urls.add(value);
        }
      }
      if (node.localName === 'style') {
        for (const match of node.textContent.matchAll(/blob:[^\s)'";]+/g)) urls.add(match[0]);
      }
    }
  }

  async function renderOffice(message, current) {
    if (!window.kynxaOffice || typeof message.contentUrl !== 'string') throw new Error('Office preview unavailable');
    const read = new AbortController(); documentRead = read;
    const saved = states.get(key) || readState(message.state);
    let reportsBackground = false;
    const background = pending => {
      if (revision === current) post({ type: pending ? 'preparing' : 'settled', key: message.key || '', renderId: message.renderId });
    };
    const handle = await window.kynxaOffice.open({
      kind: message.kind, contentUrl: message.contentUrl, container: content,
      title: message.title, state: saved, signal: read.signal,
      onState(state) {
        if (revision !== current) return;
        const merged = { ...state, x: scrollX, y: scrollY };
        states.set(message.key || '', merged);
        post({ type: 'state', key: message.key || '', state: merged });
      },
      onError() {
        if (revision === current) post({ type: message.kind + '-failed', key: message.key || '', renderId: message.renderId });
      },
      onBackground(pending) { reportsBackground = true; background(pending); },
      isCurrent: () => revision === current
    });
    if (revision !== current) { handle?.dispose?.(); return false; }
    officeView = handle;
    if (handle?.settled && !reportsBackground) {
      background(true);
      Promise.resolve(handle.settled).then(() => { if (!reportsBackground) background(false); }, () => {
        if (!reportsBackground) background(false);
        if (revision === current) post({ type: message.kind + '-failed', key: message.key || '', renderId: message.renderId });
      });
    }
    return true;
  }

  async function renderDocxBytes(bytes, options) {
    if (!window.docx) throw new Error('DOCX renderer unavailable');
    const isCurrent = options.isCurrent;

    // The renderer runs in the trusted parent; document markup lives in a scriptless, network-isolated frame.
    // 渲染器在可信父页面运行；文档标记放入禁脚本、禁外部网络的隔离框架，不共享主页面样式。
    const frame = document.createElement('iframe');
    frame.className = 'docx-view'; frame.title = options.title || 'DOCX';
    frame.setAttribute('sandbox', 'allow-same-origin');
    const view = { frame, observer: null, urls: new Set(), cleanups: [], scrollTimer: 0, disposed: false }; docxView = view;
    const alive = () => {
      if (!isCurrent() || view.disposed || options.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    };
    const loaded = new Promise((resolve, reject) => {
      const abort = () => { releaseDocx(view); reject(new DOMException('Aborted', 'AbortError')); };
      frame.addEventListener('load', resolve, { once: true });
      options.signal?.addEventListener('abort', abort, { once: true });
      view.cleanups.push(() => options.signal?.removeEventListener('abort', abort));
    });
    frame.srcdoc = `<!doctype html><html><head><meta charset="utf-8">
      <meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src blob: data:; font-src blob: data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'">
      <style>html,body{margin:0;padding:0;min-height:100%}html{overflow:auto;overscroll-behavior:contain;scrollbar-color:#aaa7a1 transparent;scrollbar-width:thin}body{font:15px/1.6 'Segoe UI Variable Text','Segoe UI','Microsoft YaHei UI','Microsoft YaHei',sans-serif;background:#f3f2f0;color:#262626;touch-action:pan-x pan-y;outline:none}
      .docx-wrapper{padding:12px!important;align-items:center}.docx-wrapper>section.docx{margin-bottom:12px!important;box-shadow:0 1px 5px #0002!important;content-visibility:auto;contain-intrinsic-size:auto 1123px}
      table{border-collapse:collapse}img{max-width:100%}a{color:inherit;text-underline-offset:3px}a:focus-visible{outline:1px solid #666;outline-offset:2px}</style></head><body tabindex="0"></body></html>`;
    content.append(frame);
    await loaded;
    alive();
    if (!frame.contentDocument) throw new DOMException('Aborted', 'AbortError');
    const frameDocument = frame.contentDocument, documentBody = frameDocument.body;
    options.onBackground?.(true);
    try {
      const renderOptions = {
        className: 'docx', inWrapper: true, breakPages: true, ignoreLastRenderedPageBreak: false,
        renderAltChunks: false, renderComments: false, experimental: false, useBase64URL: false, ignoreFonts: true
      };
      const parsed = await window.docx.parseAsync(bytes, renderOptions); alive();
      const createUrl = parsed.blobToURL.bind(parsed);
      parsed.blobToURL = (blob, path) => {
        if (view.disposed || options.signal?.aborted) return null;
        const url = createUrl(blob, path);
        if (typeof url === 'string' && url.startsWith('blob:')) view.urls.add(url);
        return url;
      };
      let wrapper, resolveMounted, rejectMounted;
      const mounted = new Promise((resolve, reject) => { resolveMounted = resolve; rejectMounted = reject; });
      // The public element factory exposes completed page DOM before the library waits for image tasks.
      // 公开元素工厂在页面 DOM 生成时即挂载，避免库内部等待全部图片后才交出首屏；第三方代码不改写。
      function createElement(specification) {
        alive();
        if (typeof specification === 'string' || typeof specification === 'number') return frameDocument.createTextNode(String(specification));
        if (specification?.nodeType) return specification;
        const { ns, tagName, className, style, children, ...properties } = specification;
        if (tagName === '#comment') return frameDocument.createComment(children?.[0] || '');
        const node = tagName === '#fragment' ? frameDocument.createDocumentFragment()
          : ns ? frameDocument.createElementNS(ns, tagName) : frameDocument.createElement(tagName);
        if (className) node.setAttribute('class', className);
        if (style) { if (typeof style === 'string') node.setAttribute('style', style); else Object.assign(node.style, style); }
        for (const [name, value] of Object.entries(properties)) {
          if (value !== undefined && !/^on/i.test(name) && !['innerHTML', 'outerHTML', 'srcdoc'].includes(name)) node[name] = value;
        }
        if (tagName === 'img') { node.loading = 'lazy'; node.decoding = 'async'; }
        for (const item of children || []) node.appendChild(createElement(item));
        if (tagName === 'style') frameDocument.head.append(node);
        if (className?.split(/\s+/).includes('docx-wrapper')) {
          wrapper = node; cleanDocxDocument(wrapper, view.urls); documentBody.append(wrapper); resolveMounted(wrapper);
        }
        return node;
      }
      const rendering = window.docx.renderDocument(parsed, { ...renderOptions, h: createElement });
      rendering.catch(rejectMounted);
      await mounted; alive();
      if (!wrapper?.querySelector('section.docx')) throw new Error('DOCX document has no rendered pages');
      let currentScale = 1;
      const fitPages = () => {
        if (view.disposed || !isCurrent() || !frame.isConnected) return;
        const pages = [...wrapper.querySelectorAll('section.docx')];
        const pageWidth = Math.max(...pages.map(page => page.offsetWidth), 1) + 24;
        const availableWidth = frameDocument.documentElement.clientWidth || frame.clientWidth;
        const scale = Math.min(1, availableWidth / pageWidth);
        if (scale > 0 && Math.abs(scale - currentScale) > .0001) {
          const ratio = scale / currentScale, x = frame.contentWindow.scrollX, y = frame.contentWindow.scrollY;
          wrapper.style.zoom = String(scale); currentScale = scale;
          frame.contentWindow.scrollTo(x * ratio, y * ratio);
        }
      };
      view.observer = new ResizeObserver(fitPages);
      view.observer.observe(frame); view.observer.observe(wrapper); fitPages();
      const getState = () => ({ docx: { x: frame.contentWindow?.scrollX || 0, y: frame.contentWindow?.scrollY || 0 } });
      const onScroll = () => {
        clearTimeout(view.scrollTimer);
        view.scrollTimer = setTimeout(() => { if (!view.disposed && isCurrent()) options.onState?.(getState()); }, 120);
      };
      frame.contentWindow.addEventListener('scroll', onScroll, { passive: true });
      view.cleanups.push(() => frame.contentWindow?.removeEventListener('scroll', onScroll));
      frameDocument.addEventListener('pointerdown', event => {
        frame.contentWindow.focus();
        if (!event.target.closest('a[href]')) documentBody.focus({ preventScroll: true });
      });
      frameDocument.addEventListener('click', event => {
        const link = event.target.closest('a[href]');
        if (!link || link.getAttribute('href')?.startsWith('#')) return;
        event.preventDefault();
        if (isCurrent() && /^https?:\/\//i.test(link.href)) post({ type: 'link', key, url: link.href });
      });
      const saved = options.state?.docx || options.state;
      if (Number.isFinite(saved?.x) && Number.isFinite(saved?.y)) frame.contentWindow.scrollTo(saved.x, saved.y);
      await new Promise(resolve => requestAnimationFrame(resolve)); alive();
      const fonts = async () => {
        const tasks = [];
        for (const font of parsed.fontTablePart?.fonts || []) for (const reference of font.embedFontRefs || []) {
          tasks.push((async () => {
            alive(); const url = await parsed.loadFont(reference.id, reference.key); alive();
            if (typeof url !== 'string' || !url.startsWith('blob:')) return;
            const face = new frame.contentWindow.FontFace(font.name, `url("${url}")`, {
              weight: ['bold', 'boldItalic'].includes(reference.type) ? '700' : '400',
              style: ['italic', 'boldItalic'].includes(reference.type) ? 'italic' : 'normal'
            });
            await face.load(); alive(); frameDocument.fonts.add(face);
          })());
        }
        await Promise.allSettled(tasks);
      };
      const settled = (async () => {
        await Promise.all([rendering, fonts()]); alive();
        cleanDocxDocument(documentBody, view.urls); fitPages();
        await frameDocument.fonts.ready; alive();
        await contentSettled(documentBody, { visibleOnly: true, signal: options.signal }); alive();
        fitPages();
      })().catch(error => {
        if (error?.name !== 'AbortError' && !view.disposed && isCurrent()) options.onError?.(error);
      }).finally(() => { if (!view.disposed && isCurrent()) options.onBackground?.(false); });
      return { dispose: () => releaseDocx(view), getState, settled };
    } catch (error) { releaseDocx(view); options.onBackground?.(false); throw error; }
  }

  window.kynxaOffice?.register('docx', renderDocxBytes);

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
