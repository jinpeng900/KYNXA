import { element, button } from './preview-ui.js';

// Compact feedback belongs to the preview that created it. / 提示跟随当前预览，关闭时释放监听。
export function createNotice(parent, className = 'preview-notice') {
  const node = element('div', className), text = element('span'), close = button('×', hide, '关闭提示');
  let error = false, disposed = false;
  const document = parent.ownerDocument;
  Object.assign(node.style, { display: 'none', alignItems: 'center', gap: '4px', minWidth: '0', fontSize: '12px' });
  Object.assign(text.style, { flex: '1', minWidth: '0', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' });
  Object.assign(close.style, { flex: 'none', minWidth: '20px', minHeight: '20px', padding: '1px 4px',
    border: '0', background: 'transparent', color: 'inherit', borderRadius: '6px' });
  close.setAttribute('aria-label', '关闭提示'); close.hidden = true;
  node.setAttribute('role', 'status'); node.setAttribute('aria-live', 'polite'); node.append(text, close); parent.append(node);
  function hide() { error = false; node.style.display = 'none'; node.classList.remove('error'); text.textContent = ''; node.removeAttribute('title'); }
  function outside(event) { if (error && !node.contains(event.target)) hide(); }
  document.addEventListener('pointerdown', outside, true);
  return {
    node,
    show(message, isError = false) {
      if (disposed) return;
      const value = String(message || '').replace(/[\r\n]+/g, ' ');
      if (!value) { hide(); return; }
      error = isError; text.textContent = value; node.title = value; close.hidden = !isError;
      node.classList.toggle('error', isError); node.style.color = isError ? 'var(--appearance-error, #b84a4a)' : '';
      node.style.display = 'flex';
    },
    hide,
    dispose() { if (disposed) return; disposed = true; document.removeEventListener('pointerdown', outside, true); node.remove(); }
  };
}
