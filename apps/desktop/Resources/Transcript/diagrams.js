/* Diagram presentation owns only local rendering and viewing, never chat data or model execution.
 * 图表展示仅负责本地渲染与查看，不拥有聊天数据，也不执行模型给出的动作。 */
(() => {
  'use strict';
  const MAX_SOURCE_CHARACTERS = 50000;
  const MAX_SVG_CHARACTERS = 2000000;
  const MAX_SVG_NODES = 16000;
  const RENDER_TIMEOUT_MS = 12000;
  const SVG_NAMESPACE = 'http://www.w3.org/2000/svg';
  const COPY_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" aria-hidden="true"><rect x="4" y="8" width="12" height="12" rx="2.5"/><path d="M8 8V6a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2h-2"/></svg>';
  const SUCCESS_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="m5 12 4 4L19 6"/></svg>';
  const ICONS = {
    'zoom-in': '<circle cx="10" cy="10" r="6.5"/><path d="m15 15 5 5M7 10h6M10 7v6"/>',
    'zoom-out': '<circle cx="10" cy="10" r="6.5"/><path d="m15 15 5 5M7 10h6"/>',
    fit: '<path d="M8 3H3v5M16 3h5v5M3 16v5h5M21 16v5h-5M8 8h8v8H8z"/>',
    fullscreen: '<path d="M8 3H3v5M16 3h5v5M3 16v5h5M21 16v5h-5"/>',
    close: '<path d="m5 5 14 14M19 5 5 19"/>'
  };
  const strings = {
    diagramWaiting: '图表代码尚未完整，等待输出…', diagramRendering: '正在绘制图表…',
    diagramFailed: '图表暂时无法绘制，原代码已保留。', diagramUnsafe: '此图表包含不支持的配置或交互，原代码已保留。',
    diagramTooLarge: '图表较大，请拆成几个较小的图表。', diagramUnsupported: '暂不支持此 Mermaid 图表类型，原代码已保留。',
    diagramSource: '查看源码', diagramHideSource: '收起源码', diagramCopySource: '复制图表源码',
    diagramZoomIn: '放大', diagramZoomOut: '缩小', diagramFit: '适应窗口', diagramFullscreen: '全屏查看',
    diagramClose: '关闭', diagramViewport: 'Mermaid 图表，可缩放和平移',
    diagramViewerHint: '拖动平移 · Ctrl＋滚轮缩放 · ＋／－缩放 · 0 适应窗口 · Esc 关闭',
    copied: '已复制', copyFailed: '复制失败，请重试。'
  };
  const states = new WeakMap();
  const records = new Set();
  const pendingCopies = new Map();
  const queue = [];
  const queued = new Set();
  let hooks = {}, palette = {}, paletteRevision = 0, renderer = null, rendererReady = null;
  let activeRequest = null, isDraining = false, sequence = 0, blockSequence = 0, copySequence = 0, viewer = null;
  const nonce = Math.random().toString(36).slice(2) + '-' + Date.now().toString(36);

  function configure(options) { hooks = { ...hooks, ...options }; }
  function translated(key) { return strings[key] || key; }
  function sourceValidation(source) {
    if (source.length > MAX_SOURCE_CHARACTERS) return 'diagramTooLarge';
    const inspected = source.replace(/<<[\p{L}\p{N}_ -]+>>/gu, '');
    // Preflight recognizes executable features, not ordinary node names or comparison labels.
    // 预检查识别可执行功能，不能把普通节点名或比较表达式误当成交互命令与 HTML。
    const unsafeHtml = /<\s*\/?\s*(?:script|style|iframe|object|embed|img|svg|foreignObject|link|meta|base|form|input|button|video|audio|canvas|body|html)(?=[\s/>])[^<>]*>|<[^<>]+\s(?:on[\w-]+|href|src)\s*=/i;
    const interaction = diagramType(source) !== 'mindmap'
      && (/^\s*click\s+[\w.-]+\s+(?:call\b|href\b|["']|[\w.$]+)/m.test(inspected)
        || /^\s*links?\s+[\w.-]+\s*:/m.test(inspected));
    if (interaction || unsafeHtml.test(inspected)
      || /%%\s*\{|^\s*---(?:\s|$)|(?:javascript|vbscript)\s*:|data\s*:[^\s]*[;,]|@import|url\s*\(|@\{[^}]*\b(?:img|icon)\s*:/im.test(inspected)) return 'diagramUnsafe';
    const declaration = source.replace(/^\s*%%[^\r\n]*(?:\r?\n|$)/gm, '').trimStart();
    if (!/^(?:flowchart\b|graph\b|sequenceDiagram\b|stateDiagram-v2\b|classDiagram\b|erDiagram\b|gantt\b|mindmap\b)/.test(declaration)) return 'diagramUnsupported';
    return null;
  }
  function diagramType(source) {
    return source.replace(/^\s*%%[^\r\n]*(?:\r?\n|$)/gm, '').trimStart().match(/^([\w-]+)/)?.[1] || 'Mermaid';
  }
  function actionButton(action, key) {
    const button = document.createElement('button');
    button.type = 'button'; button.dataset.diagramAction = action; button.dataset.diagramLabel = key;
    button.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' + ICONS[action] + '</svg>';
    button.title = translated(key); button.setAttribute('aria-label', translated(key));
    return button;
  }
  function setState(state, status, errorKey = '') {
    state.block.dataset.diagramState = status;
    if (errorKey) state.block.dataset.diagramError = errorKey;
    else delete state.block.dataset.diagramError;
    state.statusKey = errorKey || (status === 'waiting' ? 'diagramWaiting' : status === 'rendering' ? 'diagramRendering' : '');
    state.status.textContent = state.statusKey ? translated(state.statusKey) : '';
    state.status.hidden = !state.statusKey;
    state.viewport.hidden = !state.svg;
    for (const button of state.toolbar.querySelectorAll('[data-diagram-action]')) button.disabled = !state.svg;
    if (status === 'waiting' || status === 'error') state.details.open = true;
  }
  function createState(block) {
    const source = block.querySelector('pre.mermaid-source > code.language-mermaid');
    if (!source) return null;
    block.dataset.diagramId = String(++blockSequence);
    const header = document.createElement('div'); header.className = 'mermaid-header'; header.dataset.copyIgnore = '';
    const label = document.createElement('span'); label.className = 'mermaid-label';
    const toolbar = document.createElement('div'); toolbar.className = 'mermaid-toolbar'; toolbar.setAttribute('role', 'group');
    const copy = document.createElement('button'); copy.type = 'button'; copy.className = 'mermaid-copy-source'; copy.innerHTML = COPY_ICON;
    const copyStatus = document.createElement('span'); copyStatus.className = 'mermaid-copy-status'; copyStatus.dataset.copyIgnore = '';
    copyStatus.setAttribute('role', 'status'); copyStatus.setAttribute('aria-live', 'polite');
    toolbar.append(copy, actionButton('zoom-out', 'diagramZoomOut'), actionButton('zoom-in', 'diagramZoomIn'), actionButton('fit', 'diagramFit'), actionButton('fullscreen', 'diagramFullscreen'));
    const zoomLabel = document.createElement('span'); zoomLabel.className = 'mermaid-zoom-label'; zoomLabel.textContent = '100%';
    toolbar.insertBefore(zoomLabel, toolbar.children[3]);
    copy.after(copyStatus);
    header.append(label, toolbar);
    const status = document.createElement('p'); status.className = 'mermaid-status'; status.dataset.copyIgnore = ''; status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
    const viewport = document.createElement('div'); viewport.className = 'mermaid-viewport'; viewport.tabIndex = 0; viewport.hidden = true;
    viewport.setAttribute('role', 'img'); viewport.setAttribute('aria-label', translated('diagramViewport'));
    const canvas = document.createElement('div'); canvas.className = 'mermaid-canvas'; viewport.append(canvas);
    const details = document.createElement('details'); details.className = 'mermaid-source-details'; details.open = true;
    const summary = document.createElement('summary'); summary.dataset.copyIgnore = ''; details.append(summary, source.parentElement);
    block.append(header, status, viewport, details);
    const state = { block, source, header, label, toolbar, copy, copyStatus, zoomLabel, status, viewport, canvas, details, summary,
      sourceText: null, revision: 0, renderedPaletteRevision: -1, svg: null, copyTimer: 0, sourceWasToggled: false };
    state.view = createView(viewport, canvas, zoomLabel);
    details.addEventListener('toggle', () => {
      summary.textContent = translated(details.open ? 'diagramHideSource' : 'diagramSource');
    });
    summary.addEventListener('click', () => { state.sourceWasToggled = true; });
    copy.addEventListener('click', () => requestCopy(state));
    toolbar.addEventListener('click', event => {
      const button = event.target.closest('[data-diagram-action]');
      if (!button || button.disabled) return;
      if (button.dataset.diagramAction === 'fullscreen') openViewer(state, button);
      else handleViewAction(state.view, button.dataset.diagramAction);
    });
    states.set(block, state); records.add(state); localize(state);
    return state;
  }
  function localize(state) {
    state.summary.textContent = translated(state.details.open ? 'diagramHideSource' : 'diagramSource');
    state.status.textContent = state.statusKey ? translated(state.statusKey) : '';
    state.viewport.setAttribute('aria-label', translated('diagramViewport'));
    state.toolbar.setAttribute('aria-label', 'Mermaid');
    localizeCopy(state);
    for (const button of state.toolbar.querySelectorAll('[data-diagram-label]')) {
      button.title = translated(button.dataset.diagramLabel); button.setAttribute('aria-label', button.title);
    }
  }
  function localizeCopy(state) {
    const key = state.copy.dataset.copyState === 'success' ? 'copied' : state.copy.dataset.copyState === 'error' ? 'copyFailed' : 'diagramCopySource';
    if (state.copy.dataset.copyState) state.copy.removeAttribute('title');
    else state.copy.title = translated(key);
    state.copy.setAttribute('aria-label', translated(key));
    const feedback = state.copy.dataset.copyState ? translated(key) : '';
    if (state.copyStatus.textContent !== feedback) state.copyStatus.textContent = feedback;
    state.copyStatus.dataset.copyState = state.copy.dataset.copyState || '';
  }
  function hydrate(root = document) {
    disposeDetached();
    const blocks = [...(root.matches?.('.mermaid-block') ? [root] : []), ...root.querySelectorAll('.mermaid-block')];
    for (const block of blocks) {
      const state = states.get(block) || createState(block);
      if (!state || !block.isConnected) continue;
      if (!records.has(state)) {
        state.view.observer.observe(state.viewport);
        // Cached diagrams may have missed a language change while detached from the active conversation.
        // 缓存图表脱离当前聊天时可能错过语言切换，恢复展示时同步操作文案。
        localize(state);
      }
      records.add(state);
      const source = state.source.textContent;
      const ready = block.dataset.mermaidReady === 'true';
      if (source !== state.sourceText || ready !== state.ready) {
        state.sourceText = source; state.ready = ready; state.revision++; state.pendingSvg = null;
        state.label.textContent = 'Mermaid · ' + diagramType(source);
        if (state.svg) { state.svg = null; state.canvas.replaceChildren(); state.viewport.hidden = true; }
        if (viewer?.state === state) closeViewer();
        if (!ready) setState(state, 'waiting');
        else {
          const invalid = sourceValidation(source);
          if (invalid) setState(state, 'error', invalid);
          else schedule(state);
        }
      } else if (state.pendingSvg) applyResult(state, state.pendingSvg);
      else if (ready && state.renderedPaletteRevision !== paletteRevision && !state.block.dataset.diagramError) schedule(state);
      if (state.svg) synchronizeViewportSize(state.view);
    }
  }
  function schedule(state) {
    if (!state.block.isConnected || !state.ready || sourceValidation(state.sourceText)) return;
    if (state.inFlight?.revision === state.revision && state.inFlight.appearanceRevision === paletteRevision) return;
    if (!queued.has(state)) { queued.add(state); queue.push(state); }
    if (!state.svg) setState(state, 'rendering');
    drainQueue();
  }
  async function drainQueue() {
    if (isDraining) return;
    isDraining = true;
    try {
      while (queue.length) {
        const state = queue.shift(); queued.delete(state);
        if (!state.block.isConnected || !state.ready) continue;
        const revision = state.revision, appearanceRevision = paletteRevision;
        const attempt = { revision, appearanceRevision }; state.inFlight = attempt;
        try {
          const svgText = await renderSource(state.sourceText);
          if (!state.block.isConnected || state.revision !== revision || !state.ready) continue;
          if (appearanceRevision !== paletteRevision) { schedule(state); continue; }
          const svg = sanitizeSvg(svgText);
          if (!svg) throw new Error('invalid-svg');
          const result = { svg, revision, appearanceRevision };
          if (!applyResult(state, result)) state.pendingSvg = result;
        } catch {
          if (state.block.isConnected && state.revision === revision && appearanceRevision === paletteRevision) {
            // A failed repaint keeps a previously valid image and always retains its source.
            // 重绘失败时保留已有有效图像，并始终保留原始源码。
            const result = { revision, appearanceRevision, error: 'diagramFailed' };
            if (!applyResult(state, result)) state.pendingSvg = result;
          }
        } finally { if (state.inFlight === attempt) state.inFlight = null; }
      }
    } finally { isDraining = false; }
  }
  function applyResult(state, result) {
    if (!state.block.isConnected || state.revision !== result.revision || result.appearanceRevision !== paletteRevision) return true;
    if (hooks.beforeLayoutChange?.(state.block) === false) return false;
    if (result.error) {
      state.pendingSvg = null; state.renderedPaletteRevision = result.appearanceRevision;
      setState(state, 'error', result.error); hooks.afterLayoutChange?.(state.block); return true;
    }
    const previousSvg = state.svg;
    state.svg = result.svg; state.pendingSvg = null; state.renderedPaletteRevision = result.appearanceRevision;
    state.canvas.replaceChildren(state.svg);
    state.viewport.hidden = false;
    setViewSize(state.view, state.svg, !previousSvg);
    setState(state, 'ready');
    if (!previousSvg && !state.sourceWasToggled) state.details.open = false;
    if (viewer?.state === state) {
      viewer.canvas.replaceChildren(cloneSvg(state.svg));
      setViewSize(viewer.view, viewer.canvas.firstElementChild, false);
    }
    hooks.afterLayoutChange?.(state.block);
    return true;
  }
  function createRenderer() {
    if (renderer) return rendererReady;
    const frame = document.createElement('iframe');
    frame.className = 'mermaid-renderer-frame'; frame.tabIndex = -1; frame.setAttribute('aria-hidden', 'true');
    frame.setAttribute('sandbox', 'allow-scripts'); frame.setAttribute('referrerpolicy', 'no-referrer');
    rendererReady = new Promise((resolve, reject) => {
      const timer = setTimeout(() => { reject(new Error('renderer-load-timeout')); resetRenderer(); }, RENDER_TIMEOUT_MS);
      frame.readyCallback = () => { clearTimeout(timer); resolve(); };
      frame.readyTimer = timer;
    });
    renderer = frame; frame.src = 'diagram-renderer.html'; document.body.append(frame);
    return rendererReady;
  }
  function resetRenderer() {
    if (renderer) { clearTimeout(renderer.readyTimer); renderer.remove(); }
    renderer = null; rendererReady = null;
  }
  async function renderSource(source) {
    await createRenderer();
    return new Promise((resolve, reject) => {
      const token = nonce + '-' + (++sequence).toString(36);
      const timer = setTimeout(() => {
        activeRequest = null; resetRenderer(); reject(new Error('render-timeout'));
      }, RENDER_TIMEOUT_MS);
      activeRequest = { token, resolve, reject, timer };
      renderer.contentWindow.postMessage({ type: 'kynxa-mermaid-render', token, source, palette }, '*');
    });
  }
  window.addEventListener('message', event => {
    if (!renderer || event.source !== renderer.contentWindow) return;
    if (event.data?.type === 'kynxa-mermaid-ready') { renderer.readyCallback?.(); renderer.readyCallback = null; return; }
    const result = event.data;
    if (result?.type !== 'kynxa-mermaid-result' || !activeRequest || result.token !== activeRequest.token) return;
    const request = activeRequest; activeRequest = null; clearTimeout(request.timer);
    if (typeof result.svg === 'string' && result.svg.length <= MAX_SVG_CHARACTERS && !result.error) request.resolve(result.svg);
    else request.reject(new Error('render-failed'));
  });

  const SVG_ELEMENTS = new Set(['svg', 'g', 'defs', 'marker', 'path', 'rect', 'circle', 'ellipse', 'line', 'polygon', 'polyline', 'text', 'tspan', 'title', 'desc', 'style', 'clipPath', 'linearGradient', 'radialGradient', 'stop', 'pattern']);
  const SVG_ATTRIBUTES = new Set(['id', 'class', 'viewBox', 'preserveAspectRatio', 'width', 'height', 'x', 'y', 'x1', 'x2', 'y1', 'y2', 'cx', 'cy', 'r', 'rx', 'ry', 'd', 'points', 'transform', 'fill', 'fill-opacity', 'fill-rule', 'stroke', 'stroke-width', 'stroke-opacity', 'stroke-dasharray', 'stroke-dashoffset', 'stroke-linecap', 'stroke-linejoin', 'stroke-miterlimit', 'opacity', 'font-family', 'font-size', 'font-weight', 'font-style', 'text-anchor', 'dominant-baseline', 'alignment-baseline', 'dy', 'dx', 'textLength', 'lengthAdjust', 'marker-start', 'marker-mid', 'marker-end', 'markerWidth', 'markerHeight', 'refX', 'refY', 'orient', 'markerUnits', 'clip-path', 'clipPathUnits', 'gradientUnits', 'gradientTransform', 'offset', 'stop-color', 'stop-opacity', 'patternUnits', 'patternContentUnits', 'patternTransform', 'role', 'aria-label', 'aria-labelledby', 'aria-describedby', 'style', 'xmlns']);
  const CSS_PROPERTIES = new Set(['fill', 'fill-opacity', 'fill-rule', 'stroke', 'stroke-width', 'stroke-opacity', 'stroke-dasharray', 'stroke-dashoffset', 'stroke-linecap', 'stroke-linejoin', 'stroke-miterlimit', 'opacity', 'font-family', 'font-size', 'font-weight', 'font-style', 'font-variant', 'text-anchor', 'text-decoration', 'dominant-baseline', 'alignment-baseline', 'paint-order', 'color', 'background-color', 'white-space', 'word-spacing', 'letter-spacing', 'line-height', 'marker-start', 'marker-mid', 'marker-end', 'clip-path', 'rx', 'ry', 'filter', 'text-overflow', 'overflow', 'max-width', 'min-width', 'width', 'height', 'display', 'visibility']);
  function safeReference(value, ids) {
    if (/javascript|vbscript|expression|@import|(?:https?|data|file|ftp)\s*:|[<>]/i.test(value)) return null;
    let invalid = false;
    const rewritten = value.replace(/url\(\s*['"]?#([^\s'"()]+)['"]?\s*\)/gi, (_, id) => {
      if (!ids.has(id)) { invalid = true; return ''; }
      return 'url(#' + ids.get(id) + ')';
    });
    if (invalid || /url\s*\(/i.test(rewritten.replace(/url\(#[\w-]+\)/gi, '')) || /\\/.test(rewritten)) return null;
    return rewritten;
  }
  function sanitizeDeclarations(declarations, ids) {
    const safe = document.createElement('span').style;
    for (const property of declarations) {
      if (!CSS_PROPERTIES.has(property) || property === 'filter') continue;
      const value = safeReference(declarations.getPropertyValue(property), ids);
      if (value !== null && value.length < 1000) safe.setProperty(property, value);
    }
    return safe.cssText;
  }
  function sanitizeStyleText(css, originalId, ids) {
    if (!originalId || css.length > 500000) return '';
    const sheet = new CSSStyleSheet();
    try { sheet.replaceSync(css); } catch { return ''; }
    const prefix = '#' + CSS.escape(originalId), rules = [];
    for (const rule of sheet.cssRules) {
      if (rule.type !== CSSRule.STYLE_RULE) continue;
      const selectors = rule.selectorText.split(',').map(selector => selector.trim());
      if (selectors.some(selector => !selector.startsWith(prefix) || !/^(?:\s|[.:[>]|$)/.test(selector.slice(prefix.length)) || /[+~]|:has\(/.test(selector))) continue;
      const rewritten = selectors.map(selector => selector.replace(/#([\p{L}\p{N}_:.-]+)/gu, (match, id) => ids.has(id) ? '#' + ids.get(id) : match));
      const style = sanitizeDeclarations(rule.style, ids);
      if (style) rules.push(rewritten.join(',') + '{' + style + '}');
    }
    return rules.join('\n');
  }
  function sanitizeSvg(svgText) {
    if (typeof svgText !== 'string' || svgText.length > MAX_SVG_CHARACTERS || /<!DOCTYPE|<!ENTITY/i.test(svgText)) return null;
    const parsed = new DOMParser().parseFromString(svgText, 'image/svg+xml');
    const svg = parsed.documentElement;
    if (svg.localName !== 'svg' || svg.namespaceURI !== SVG_NAMESPACE || parsed.querySelector('parsererror')) return null;
    const elements = [svg, ...svg.querySelectorAll('*')];
    if (elements.length > MAX_SVG_NODES) return null;
    const originalId = svg.id, ids = new Map(), prefix = 'diagram-safe-' + (++sequence) + '-';
    for (const element of elements) {
      if (element.id && !ids.has(element.id)) ids.set(element.id, prefix + ids.size);
    }
    // Rebuild a conservative SVG tree. No foreignObject, links, animation, events or external references survive.
    // 重建保守的 SVG 节点树；不保留 foreignObject、链接、动画、事件或外部引用。
    function copyElement(element) {
      if (element.namespaceURI !== SVG_NAMESPACE || !SVG_ELEMENTS.has(element.localName)) return null;
      const result = document.createElementNS(SVG_NAMESPACE, element.localName);
      if (element.localName === 'style') {
        result.textContent = sanitizeStyleText(element.textContent, originalId, ids);
        return result.textContent ? result : null;
      }
      for (const attribute of element.attributes) {
        const name = attribute.name;
        if (!SVG_ATTRIBUTES.has(name) || attribute.namespaceURI && name !== 'xmlns') continue;
        let value = attribute.value;
        if (name === 'id') value = ids.get(value) || '';
        else if (name === 'style') value = sanitizeDeclarations(element.style, ids);
        else if (name === 'aria-labelledby' || name === 'aria-describedby') value = value.split(/\s+/).filter(id => ids.has(id)).map(id => ids.get(id)).join(' ');
        else value = safeReference(value, ids);
        if (value !== null && value.length <= 200000) result.setAttribute(name, value);
      }
      for (const child of element.childNodes) {
        if (child.nodeType === Node.TEXT_NODE) result.append(document.createTextNode(child.textContent));
        else if (child.nodeType === Node.ELEMENT_NODE) {
          const safe = copyElement(child); if (safe) result.append(safe);
        }
      }
      return result;
    }
    const clean = copyElement(svg);
    if (!clean?.querySelector('path,rect,circle,ellipse,line,polygon,polyline,text')) return null;
    const box = clean.getAttribute('viewBox')?.trim().split(/[\s,]+/).map(Number);
    if (!box || box.length !== 4 || box.some(value => !Number.isFinite(value)) || box[2] <= 0 || box[3] <= 0 || box[2] > 100000 || box[3] > 100000) return null;
    clean.classList.add('mermaid-svg'); clean.setAttribute('role', 'img'); clean.setAttribute('aria-label', translated('diagramViewport'));
    clean.removeAttribute('width'); clean.removeAttribute('height');
    clean.style.width = '100%'; clean.style.height = '100%'; clean.style.maxWidth = 'none';
    return clean;
  }

  function createView(viewport, canvas, zoomLabel) {
    const view = { viewport, canvas, zoomLabel, width: 1, height: 1, scale: 1, x: 0, y: 0, fitted: true,
      viewportWidth: 0, viewportHeight: 0, pendingInlineHeight: false, pointers: new Map(), pinch: null };
    const observer = new ResizeObserver(() => synchronizeViewportSize(view)); observer.observe(viewport); view.observer = observer;
    viewport.addEventListener('wheel', event => {
      if (!event.ctrlKey) return;
      event.preventDefault();
      const bounds = viewport.getBoundingClientRect();
      zoomAt(view, Math.exp(-event.deltaY * .002), event.clientX - bounds.left, event.clientY - bounds.top);
    }, { passive: false });
    viewport.addEventListener('keydown', event => {
      const movements = { ArrowLeft: [35, 0], ArrowRight: [-35, 0], ArrowUp: [0, 35], ArrowDown: [0, -35] };
      if (movements[event.key]) { event.preventDefault(); view.fitted = false; view.x += movements[event.key][0]; view.y += movements[event.key][1]; updateTransform(view); }
      else if (event.key === '+' || event.key === '=') { event.preventDefault(); zoomAt(view, 1.25); }
      else if (event.key === '-') { event.preventDefault(); zoomAt(view, .8); }
      else if (event.key === '0' || event.key === 'Home') { event.preventDefault(); fitView(view); }
    });
    viewport.addEventListener('pointerdown', event => {
      if (event.button !== 0 || event.target.closest('button') || event.pointerType === 'mouse' && !event.altKey && event.target.closest('text,tspan')) return;
      view.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      viewport.setPointerCapture(event.pointerId); viewport.dataset.panning = 'true';
      if (view.pointers.size === 2) view.pinch = pinchSnapshot(view);
    });
    viewport.addEventListener('pointermove', event => {
      const previous = view.pointers.get(event.pointerId);
      if (!previous) return;
      view.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
      if (view.pointers.size >= 2) {
        const current = pinchSnapshot(view);
        if (view.pinch && current) {
          const bounds = viewport.getBoundingClientRect();
          zoomAt(view, current.distance / Math.max(1, view.pinch.distance), current.x - bounds.left, current.y - bounds.top);
          view.x += current.x - view.pinch.x; view.y += current.y - view.pinch.y; updateTransform(view);
        }
        view.pinch = current;
      } else {
        view.fitted = false; view.x += event.clientX - previous.x; view.y += event.clientY - previous.y; updateTransform(view);
      }
    });
    const endPointer = event => {
      view.pointers.delete(event.pointerId); view.pinch = null;
      if (!view.pointers.size) delete viewport.dataset.panning;
      if (viewport.hasPointerCapture(event.pointerId)) viewport.releasePointerCapture(event.pointerId);
    };
    viewport.addEventListener('pointerup', endPointer); viewport.addEventListener('pointercancel', endPointer);
    viewport.addEventListener('lostpointercapture', event => { view.pointers.delete(event.pointerId); if (!view.pointers.size) delete viewport.dataset.panning; });
    return view;
  }
  function pinchSnapshot(view) {
    const [first, second] = [...view.pointers.values()];
    return first && second ? { distance: Math.hypot(first.x - second.x, first.y - second.y), x: (first.x + second.x) / 2, y: (first.y + second.y) / 2 } : null;
  }
  function setViewSize(view, svg, fit) {
    const box = svg.getAttribute('viewBox').split(/[\s,]+/).map(Number);
    view.width = box[2]; view.height = box[3];
    view.canvas.style.width = view.width + 'px'; view.canvas.style.height = view.height + 'px';
    updateInlineHeight(view, false);
    if (fit || view.fitted) fitView(view); else updateTransform(view);
  }
  function updateInlineHeight(view, preserveLayout) {
    if (!view.viewport.classList.contains('mermaid-viewport') || !view.canvas.firstElementChild) return true;
    const width = view.viewport.clientWidth;
    if (!width) return true;
    // Compact diagrams use their natural height; tall diagrams keep a bounded, zoomable viewport.
    // 简单图表按自然高度收紧，较高图表保留有上限且可缩放的查看区域。
    const naturalScale = Math.min(1, Math.max(1, width - 24) / view.width);
    const height = Math.round(Math.max(160, Math.min(380, view.height * naturalScale + 24))) + 'px';
    if (view.viewport.style.height === height) { view.pendingInlineHeight = false; return true; }
    const block = view.viewport.closest('.mermaid-block');
    if (preserveLayout && hooks.beforeLayoutChange?.(block) === false) { view.pendingInlineHeight = true; return false; }
    view.viewport.style.height = height; view.pendingInlineHeight = false;
    if (preserveLayout) hooks.afterLayoutChange?.(block);
    return true;
  }
  function synchronizeViewportSize(view) {
    if (!view.canvas.firstElementChild || !view.pendingInlineHeight
      && view.viewportWidth === view.viewport.clientWidth && view.viewportHeight === view.viewport.clientHeight) return;
    if (!updateInlineHeight(view, true)) return;
    if (view.fitted) fitView(view); else updateTransform(view);
  }
  function fitView(view) {
    const width = view.viewport.clientWidth, height = view.viewport.clientHeight;
    if (!width || !height) return;
    view.scale = Math.max(.001, Math.min(1, (width - 24) / view.width, (height - 24) / view.height));
    view.x = (width - view.width * view.scale) / 2; view.y = (height - view.height * view.scale) / 2;
    view.fitted = true; updateTransform(view);
  }
  function zoomAt(view, factor, x = view.viewport.clientWidth / 2, y = view.viewport.clientHeight / 2) {
    const scale = Math.max(.001, Math.min(8, view.scale * factor)), ratio = scale / view.scale;
    view.x = x - (x - view.x) * ratio; view.y = y - (y - view.y) * ratio; view.scale = scale;
    view.fitted = false; updateTransform(view);
  }
  function updateTransform(view) {
    view.viewportWidth = view.viewport.clientWidth; view.viewportHeight = view.viewport.clientHeight;
    const margin = Math.min(80, view.viewport.clientWidth / 3, view.viewport.clientHeight / 3);
    view.x = Math.min(view.viewport.clientWidth - margin, Math.max(margin - view.width * view.scale, view.x));
    view.y = Math.min(view.viewport.clientHeight - margin, Math.max(margin - view.height * view.scale, view.y));
    view.canvas.style.transform = `translate(${view.x}px, ${view.y}px) scale(${view.scale})`;
    view.viewport.dataset.scale = String(view.scale); view.viewport.dataset.canPan = 'true';
    if (view.zoomLabel) view.zoomLabel.textContent = (view.scale < .1 ? (view.scale * 100).toFixed(1) : Math.round(view.scale * 100)) + '%';
  }
  function handleViewAction(view, action) {
    if (action === 'fit') fitView(view);
    else if (action === 'zoom-in') zoomAt(view, 1.25);
    else if (action === 'zoom-out') zoomAt(view, .8);
  }
  function openViewer(state, trigger) {
    if (!state.svg) return;
    closeViewer();
    const dialog = document.createElement('dialog'); dialog.className = 'mermaid-viewer'; dialog.dataset.copyIgnore = '';
    const header = document.createElement('div'); header.className = 'mermaid-viewer-header';
    const title = document.createElement('span'); title.className = 'mermaid-viewer-title'; title.textContent = state.label.textContent; title.id = 'mermaid-viewer-title';
    dialog.setAttribute('aria-labelledby', title.id);
    const toolbar = document.createElement('div'); toolbar.className = 'mermaid-toolbar';
    toolbar.append(actionButton('zoom-out', 'diagramZoomOut'), actionButton('zoom-in', 'diagramZoomIn'), actionButton('fit', 'diagramFit'), actionButton('close', 'diagramClose'));
    header.append(title, toolbar);
    const viewport = document.createElement('div'); viewport.className = 'mermaid-viewer-viewport'; viewport.tabIndex = 0;
    viewport.setAttribute('aria-label', translated('diagramViewport'));
    const canvas = document.createElement('div'); canvas.className = 'mermaid-canvas'; canvas.append(cloneSvg(state.svg)); viewport.append(canvas);
    const hint = document.createElement('div'); hint.className = 'mermaid-viewer-hint'; hint.textContent = translated('diagramViewerHint');
    dialog.append(header, viewport, hint); document.body.append(dialog);
    const view = createView(viewport, canvas, null);
    viewer = { dialog, state, trigger, canvas, view, toolbar, hint };
    toolbar.addEventListener('click', event => {
      const action = event.target.closest('[data-diagram-action]')?.dataset.diagramAction;
      if (action === 'close') closeViewer(); else if (action) handleViewAction(view, action);
    });
    dialog.addEventListener('cancel', event => { event.preventDefault(); closeViewer(); });
    dialog.addEventListener('close', () => { if (viewer?.dialog === dialog) closeViewer(); });
    dialog.showModal(); setViewSize(view, canvas.firstElementChild, true); viewport.focus({ preventScroll: true });
  }
  function cloneSvg(svg) {
    // The viewer has separate IDs so markers and clipping never resolve into a different diagram.
    // 查看器使用独立 ID，避免箭头或裁剪引用解析到另一张图表。
    return sanitizeSvg(new XMLSerializer().serializeToString(svg));
  }
  function closeViewer() {
    if (!viewer) return;
    const previous = viewer; viewer = null; previous.view.observer.disconnect();
    previous.dialog.close(); previous.dialog.remove();
    if (previous.trigger.isConnected) previous.trigger.focus({ preventScroll: true });
  }
  function requestCopy(state) {
    if (!hooks.requestCopy) return;
    const requestId = 'diagram-' + (++copySequence);
    while (pendingCopies.size >= 32) pendingCopies.delete(pendingCopies.keys().next().value);
    state.copyRequestId = requestId; pendingCopies.set(requestId, state);
    hooks.requestCopy(state.sourceText, state.block, requestId);
  }
  function acknowledgeCopy(command) {
    const state = pendingCopies.get(command?.requestId); pendingCopies.delete(command?.requestId);
    if (!state || !state.block.isConnected || state.copyRequestId !== command.requestId) return;
    clearTimeout(state.copyTimer);
    state.copy.dataset.copyState = command.success === true ? 'success' : 'error';
    state.copy.innerHTML = command.success === true ? SUCCESS_ICON : COPY_ICON; localizeCopy(state);
    state.copyTimer = setTimeout(() => { delete state.copy.dataset.copyState; state.copy.innerHTML = COPY_ICON; localizeCopy(state); }, 2200);
  }
  function setAppearance(next) {
    const nextPalette = next?.palette || next;
    const keys = ['main', 'soft', 'accent', 'selection', 'text', 'secondary', 'border', 'sidebar'];
    if (!nextPalette || keys.some(key => !/^#[0-9a-f]{6}$/i.test(nextPalette[key]))) return;
    if (keys.every(key => palette[key] === nextPalette[key])) return;
    palette = Object.fromEntries(keys.map(key => [key, nextPalette[key]])); paletteRevision++;
    for (const state of records) {
      if (!state.block.isConnected || !state.ready || sourceValidation(state.sourceText)) continue;
      state.revision++; state.pendingSvg = null; schedule(state);
    }
  }
  function setLanguage(next) {
    Object.assign(strings, next?.strings || next || {});
    for (const state of records) if (state.block.isConnected) localize(state);
    if (viewer) {
      viewer.hint.textContent = translated('diagramViewerHint'); viewer.view.viewport.setAttribute('aria-label', translated('diagramViewport'));
      for (const button of viewer.toolbar.querySelectorAll('[data-diagram-label]')) { button.title = translated(button.dataset.diagramLabel); button.setAttribute('aria-label', button.title); }
    }
  }
  function disposeDetached() {
    for (const state of records) {
      if (state.block.isConnected) continue;
      state.revision++; state.pendingSvg = null; state.view.observer.disconnect(); clearTimeout(state.copyTimer); records.delete(state);
      delete state.copy.dataset.copyState; state.copy.innerHTML = COPY_ICON; localizeCopy(state);
      if (viewer?.state === state) closeViewer();
      for (const [id, pending] of pendingCopies) if (pending === state) pendingCopies.delete(id);
    }
  }
  window.kynxaDiagrams = { configure, hydrate, setAppearance, setLanguage, acknowledgeCopy, closeViewer, disposeDetached };
})();
