/* This page renders gateway-authorized content only; document text cannot send native actions.
 * 此页只展示网关已授权内容，文档文本不能执行原生动作。 */
(() => {
  'use strict';
  const content = document.getElementById('content');
  const states = new Map();
  let key = '', renderId = null, revision = 0, scrollTimer = 0, docxView = null, officeView = null, documentRead = null, previewView = null, active = true, requestSequence = 0;
  const requests = new Map();
  const post = message => window.chrome?.webview?.postMessage(message);
  let notice = null, noticeTimer = 0;

  function clearNotice() { clearTimeout(noticeTimer); notice?.dispose(); notice = null; }
  async function feedback(text, error, scope = { key, renderId, revision }) {
    const { createNotice } = await import('./preview-notice.js');
    if (scope.key !== key || scope.renderId !== renderId || scope.revision !== revision) return;
    clearNotice(); notice = createNotice(content); notice.show(text, error);
    if (!error) noticeTimer = setTimeout(clearNotice, 4000);
  }
  function cancelRequests(message) {
    for (const pending of requests.values()) { clearTimeout(pending.timer); pending.reject(new DOMException(message, 'AbortError')); }
    requests.clear();
  }

  function saveState() {
    if (!key) return;
    const state = { ...(officeView?.getState?.() || previewView?.getState?.() || {}), x: scrollX, y: scrollY };
    states.delete(key); states.set(key, state);
    while (states.size > 24) states.delete(states.keys().next().value);
    post({ type: 'state', key, renderId, state });
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
    clearNotice();
    key = message.key || '';
    renderId = message.renderId;
    const current = ++revision;
    const requestScope = { key, renderId, revision: current };
    documentRead?.abort(); documentRead = null;
    officeView?.dispose?.(); officeView = null;
    previewView?.dispose?.(); previewView = null;
    cancelRequests('预览已切换');
    releaseDocx(docxView); docxView = null;
    content.replaceChildren();
    window.kynxaContentBlocks?.disposeDetached();
    window.kynxaTables?.disposeDetached();
    document.body.dataset.previewKind = message.kind || '';
    window.kynxaDiagrams?.disposeDetached();
    active = message.active !== false;
    if (['model3d', 'image', 'audio', 'video', 'workbench', 'delimited'].includes(message.kind)) {
      try {
        const moduleName = ({ model3d: 'model-preview', image: 'image-preview', audio: 'media-preview', video: 'media-preview', workbench: 'workbench', delimited: 'data-preview' })[message.kind];
        const module = await import('./' + moduleName + '.js');
        if (current !== revision) return;
        const read = new AbortController(); documentRead = read;
        requestScope.signal = read.signal;
        const handle = await module.open({ ...message, container: content, state: states.get(key) || readState(message.state), signal: read.signal,
          sourceFormat: message.renderFormat || message.sourceFormat,
          isCurrent: () => revision === current,
          onState(state) { if (current !== revision) return; states.set(key, state); post({ type: 'state', key, renderId: message.renderId, state }); },
          onSelection(payload) { if (current === revision) post({ type: 'selection', key, renderId: message.renderId, ...payload, payload, version: message.version }); },
          onExport(payload) { if (current === revision) post({ type: 'exportAsset', key, renderId: message.renderId, ...payload }); },
          onOpen(path) { if (current === revision) post({ type: 'link', key, url: path }); },
          onNavigate(conversationId) { if (current === revision) post({ type: 'navigateConversation', key, conversationId }); },
          onAction(action) { if (current === revision) post({ type: 'workbench-action', key, payload: { action } }); },
          request(action, payload) { return requestWorkbench(action, payload, requestScope); }
        });
        if (revision !== current) { handle?.dispose?.(); return; }
        previewView = handle; handle?.setActive?.(active);
      } catch (error) {
        if (current !== revision || error.name === 'AbortError') return;
        documentRead?.abort(); cancelRequests('预览未能打开'); content.replaceChildren();
        const text = error.message || '此内容暂时无法打开';
        await feedback(text, true, requestScope);
        if (current !== revision) return;
        post({ type: 'previewError', key: requestScope.key, renderId: message.renderId, message: text });
      }
    } else if (message.kind === 'markdown' || message.kind === 'mermaid' || message.kind === 'code') {
      // HTML is produced by the existing HTML-disabled Markdown renderer, never passed through from a document.
      // HTML 仅来自现有禁止原生 HTML 的 Markdown 渲染器，绝不直接接受文档内的 HTML。
      content.innerHTML = typeof message.html === 'string' ? message.html : '';
      for (const formula of content.querySelectorAll('.math[data-latex]')) {
        if (!window.katex) continue;
        try { window.katex.render(formula.dataset.latex || '', formula,
          { displayMode: formula.dataset.display === 'true', throwOnError: false, trust: false, strict: 'ignore' }); } catch { }
      }
      window.kynxaDiagrams?.hydrate(content);
      window.kynxaContentBlocks?.hydrate(content);
      window.kynxaTables?.hydrate(content);
    } else if (['docx', 'pptx', 'xlsx'].includes(message.kind)) {
      try {
        if (!await renderOffice(message, current)) return;
      } catch {
        if (revision === current) post({ type: message.kind + '-failed', key: message.key || '', renderId: message.renderId });
        return;
      }
    } else {
      const text = document.createElement('pre'); text.className = 'plain-text';
      text.textContent = typeof message.text === 'string' ? message.text : ''; content.append(text);
    }
    document.title = message.title || '文件预览';
    requestAnimationFrame(() => { if (revision === current) restoreState(states.get(key) || readState(message.state)); });
    // Office modules report their usable first view independently from background resource completion.
    // Office 模块将可操作首屏与后台资源完成分开回报，不让字体、图片及后续页阻塞 shown。
    if (!['docx', 'pptx', 'xlsx'].includes(message.kind)) await contentSettled(content, { visibleOnly: true });
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
          const allowed = isLink ? window.kynxaMarkdownLinks?.isSafe(value) === true : /^(blob:|data:image\/)/i.test(value);
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
        post({ type: 'state', key: message.key || '', renderId: message.renderId, state: merged });
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
        if (isCurrent()) window.kynxaMarkdownLinks?.follow(link, documentBody, url => post({ type: 'link', key, url }));
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

  function requestWorkbench(action, payload, scope) {
    // A detached preview cannot dispatch work against whichever file is now open.
    // 已切走的预览不能把旧操作发到当前文件。
    if (scope.revision !== revision || scope.key !== key || scope.renderId !== renderId || scope.signal?.aborted)
      return Promise.reject(new DOMException('预览已切换', 'AbortError'));
    const requestId = 'workbench-' + (++requestSequence);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { requests.delete(requestId); reject(new Error('工作台请求超时，请刷新状态后重试。')); }, action === 'media.transcribe' ? 610000 : 90000);
      requests.set(requestId, { resolve, reject, timer, ...scope });
      post({ type: 'workbenchRequest', key: scope.key, renderId: scope.renderId, requestId, action, payload });
    });
  }

  function appearance(palette) {
    for (const [name, value] of Object.entries(palette || {})) {
      if (/^[a-z]+$/.test(name) && /^#[0-9a-f]{6}$/i.test(value))
        document.documentElement.style.setProperty('--appearance-' + name, value);
    }
    if (/^#[0-9a-f]{6}$/i.test(palette?.main || '')) {
      const rgb = [1, 3, 5].map(i => parseInt(palette.main.slice(i, i + 2), 16));
      document.documentElement.style.colorScheme = rgb[0] * .2126 + rgb[1] * .7152 + rgb[2] * .0722 < 128 ? 'dark' : 'light';
    }
  }

  const requestCopy = (text, block, requestId) => {
    if (!key || !block.isConnected || !content.contains(block)) return false;
    post({ type: 'copy', key, text, requestId }); return true;
  };
  window.kynxaContentBlocks?.configure({ requestCopy });
  window.kynxaTables?.configure({ requestCopy });
  window.kynxaDiagrams?.configure({ requestCopy, requestExport(format, content, block, requestId) {
    if (!key || !block.isConnected) return false;
    post({ type: 'exportDiagram', key, format, content, requestId }); return true;
  } });
  window.chrome?.webview?.addEventListener('message', event => {
    const message = event.data;
    if (message?.type === 'show') show(message);
    else if (message?.type === 'setActive' && (!message.key || message.key === key)) { active = message.active !== false; previewView?.setActive?.(active); }
    else if (message?.type === 'workbenchResult') {
      const pending = requests.get(message.requestId);
      if (!pending || pending.key !== key || pending.revision !== revision || pending.renderId !== renderId
        || message.key !== pending.key || message.renderId !== pending.renderId) return;
      requests.delete(message.requestId); clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(typeof message.error === 'string' ? message.error : message.error.message || '工作台操作失败')); else pending.resolve(message.result);
    }
    else if (message?.type === 'exportAssetResult') {
      if (message.key !== key || message.renderId !== renderId) return;
      const text = message.outcome === 'success' ? '已保存' : message.outcome === 'cancelled' ? '已取消保存' : message.error || '保存失败';
      void feedback(text, message.outcome === 'error');
    }
    else if (message?.type === 'command' && message.command === 'refresh') { post({ type: 'refresh', key }); }
    else if (message?.type === 'viewport') { content.style.maxWidth = message.width > 0 ? message.width + 'px' : ''; content.style.margin = message.width > 0 ? 'auto' : ''; }
    else if (message?.type === 'saveState') saveState();
    else if (message?.type === 'appearance') {
      appearance(message.palette);
      window.kynxaContentBlocks?.setLanguage(message.strings);
      window.kynxaTables?.setLanguage(message.strings);
      window.kynxaDiagrams?.setLanguage(message.strings);
    }
    else if (message?.type === 'copyResult' && message.key === key) {
      window.kynxaContentBlocks?.acknowledgeCopy(message);
      window.kynxaTables?.acknowledgeCopy(message);
      window.kynxaDiagrams?.acknowledgeCopy(message);
    }
    else if (message?.type === 'anchor' && message.key === key) window.kynxaMarkdownLinks?.anchor(content, message.fragment || '');
    else if (message?.type === 'diagramExportResult' && message.key === key) window.kynxaDiagrams?.acknowledgeExport(message);
  });
  document.addEventListener('click', event => {
    const link = event.target.closest('a[href]');
    if (!link) return;
    event.preventDefault();
    window.kynxaMarkdownLinks?.follow(link, content, url => post({ type: 'link', key, url }));
  });
  const selectionButton = document.createElement('button'); selectionButton.className = 'preview-button'; selectionButton.textContent = '引用选中内容';
  Object.assign(selectionButton.style, { position: 'fixed', right: '14px', bottom: '14px', zIndex: '30', boxShadow: '0 3px 15px #0003' }); selectionButton.hidden = true; document.body.append(selectionButton);
  document.addEventListener('selectionchange', () => { selectionButton.hidden = !(window.getSelection()?.toString().trim()) || document.body.dataset.previewKind === 'workbench'; });
  selectionButton.addEventListener('pointerdown', event => event.preventDefault());
  selectionButton.addEventListener('click', () => { const text = window.getSelection()?.toString().trim(); if (text) post({ type: 'selection', key, renderId, kind: 'text', title: document.title, text: text.slice(0, 16000), payload: { kind: 'text', title: document.title, text: text.slice(0, 16000) } }); });
  window.addEventListener('pagehide', () => { ++revision; key = ''; renderId = null; clearTimeout(scrollTimer); clearNotice(); previewView?.dispose?.(); officeView?.dispose?.(); documentRead?.abort(); cancelRequests('预览已关闭'); });
  window.addEventListener('scroll', () => { clearTimeout(scrollTimer); scrollTimer = setTimeout(saveState, 120); }, { passive: true });
  post({ type: 'ready' });
})();
