// Shared controls for local previews. / 本地预览共用控件。
export function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = String(text);
  return node;
}
export function button(text, action, title = text) {
  const node = element('button', 'preview-button', text); node.type = 'button'; node.title = title;
  node.addEventListener('click', action); return node;
}
export function select(items, value, action) {
  const node = element('select', 'preview-select');
  for (const [id, text] of items) { const option = element('option', '', text); option.value = id; node.append(option); }
  node.value = value; node.addEventListener('change', () => action(node.value)); return node;
}
export function field(label, input) { const node = element('label', 'preview-field'); node.append(element('span', '', label), input); return node; }
export function number(value, min, max, action, step = 1) {
  const node = element('input', 'preview-number'); node.type = 'number'; node.min = min; node.max = max; node.step = step; node.value = value;
  node.addEventListener('change', () => action(Math.min(max, Math.max(min, Number(node.value) || min)))); return node;
}
export function input(placeholder = '', value = '') { const n = element('input', 'preview-input'); n.placeholder = placeholder; n.value = value; return n; }
export function toolbar(label = '预览操作') { const node = element('div', 'preview-toolbar'); node.setAttribute('role', 'toolbar'); node.setAttribute('aria-label', label); return node; }
export function group(label, ...controls) {
  const node = element('div', 'preview-group'); node.setAttribute('role', 'group'); node.setAttribute('aria-label', label); node.append(...controls); return node;
}
export function disclosure(label, ...controls) {
  const node = element('details', 'preview-disclosure'), summary = element('summary', '', label);
  node.append(summary, group(label, ...controls)); return node;
}
export function download(options, filename, content, mimeType) {
  options.onExport?.({ filename, content, mimeType });
}
export function visibleError(container, error) {
  const node = element('div', 'preview-error', error?.message || '内容暂时无法打开'); node.setAttribute('role', 'status'); container.append(node);
}
export function abortError() { return new DOMException('已取消', 'AbortError'); }
export function alive(options) { if (options.signal?.aborted || options.isCurrent && !options.isCurrent()) throw abortError(); }
