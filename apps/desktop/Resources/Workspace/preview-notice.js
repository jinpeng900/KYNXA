import { element, button } from './preview-ui.js';

// Compact feedback belongs to the preview that created it. / 提示跟随当前预览，关闭时释放监听。
export function createNotice(parent, className = 'preview-notice', { dismissAfterMs = 0 } = {}) {
  const node = element('div', className), text = element('span', 'preview-notice-text'), close = button('', hide, '关闭提示');
  const document = parent.ownerDocument, events = new AbortController();
  let error = false, disposed = false, timer = null, remaining = dismissAfterMs, started = 0, pointerInside = false;
  function svg(paths) {
    const icon = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    icon.setAttribute('viewBox', '0 0 24 24'); icon.setAttribute('aria-hidden', 'true');
    for (const data of paths) { const path = document.createElementNS(icon.namespaceURI, 'path'); path.setAttribute('d', data); icon.append(path); }
    return icon;
  }
  const mark = svg(['M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18', 'M12 11v6M12 7v.1']);
  mark.classList.add('preview-notice-icon'); close.append(svg(['M6 6l12 12M18 6L6 18']));
  close.classList.add('preview-notice-close'); close.setAttribute('aria-label', '关闭提示'); close.hidden = true;
  node.classList.add('preview-notice'); node.style.display = 'none';
  node.setAttribute('role', 'status'); node.setAttribute('aria-live', 'polite'); node.append(mark, text, close); parent.append(node);
  function pause() {
    if (timer === null) return;
    clearTimeout(timer); timer = null; remaining = Math.max(0, remaining - (performance.now() - started));
  }
  function resume() {
    pause();
    if (!dismissAfterMs || disposed || node.style.display === 'none' || pointerInside || node.contains(document.activeElement)) return;
    if (remaining <= 0) { hide(); return; }
    started = performance.now(); timer = setTimeout(hide, remaining);
  }
  function hide() { pause(); error = false; pointerInside = false; node.style.display = 'none'; node.classList.remove('error'); text.textContent = ''; node.removeAttribute('title'); }
  function outside(event) { if (error && !node.contains(event.target)) hide(); }
  document.addEventListener('pointerdown', outside, { capture: true, signal: events.signal });
  node.addEventListener('pointerenter', () => { pointerInside = true; pause(); }, { signal: events.signal });
  node.addEventListener('pointerleave', () => { pointerInside = false; resume(); }, { signal: events.signal });
  node.addEventListener('focusin', pause, { signal: events.signal });
  node.addEventListener('focusout', () => queueMicrotask(resume), { signal: events.signal });
  return {
    node,
    show(message, isError = false) {
      if (disposed) return;
      const value = String(message || '').replace(/[\r\n]+/g, ' ');
      if (!value) { hide(); return; }
      pause(); remaining = dismissAfterMs;
      error = isError; text.textContent = value; node.title = value; close.hidden = !isError && !dismissAfterMs;
      node.classList.toggle('error', isError); node.style.display = 'flex'; pointerInside = node.matches(':hover'); resume();
    },
    hide,
    dispose() { if (disposed) return; disposed = true; pause(); events.abort(); node.remove(); }
  };
}
