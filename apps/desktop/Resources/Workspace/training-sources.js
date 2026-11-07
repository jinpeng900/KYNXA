import { element as el, input, field } from './preview-ui.js';

const imageExtensions = /\.(?:png|jpe?g|webp|bmp|gif|tiff?)$/i;
const privateImport = /(?:^|\/)imports\/[0-9a-f]{8}-[0-9a-f-]{27,}\//i;
function imageSource(source) { return imageExtensions.test(source.name || source.originalPath || source.path || ''); }
function identity(source) { return source.importId || source.originalPath || source.path; }
function parentLabel(source) {
  const original = source.originalPath || source.path;
  if (typeof original !== 'string' || !original.trim()) return '';
  const path = original.replaceAll('\\', '/');
  // 私有副本目录只表示存储位置，不能拿来当分类标签。
  if (privateImport.test(path) || /\/datasets\/[0-9a-f-]+\/assets\//i.test(path)) return '';
  const parts = path.split('/').filter(Boolean), parent = parts.at(-2);
  if (path.startsWith('//') && parts.length < 4 || !parent || /^[a-z]:$/i.test(parent) || ['.', '..', '?'].includes(parent)) return '';
  return parent;
}

export function createTrainingSources(host, { getSources, onChange, control, request }) {
  const selected = new Set(), previews = new Map();
  let disposed = false, generation = 0, observer = null, queue = [], running = 0;
  function preview(node, source, version) {
    const key = identity(source);
    async function load() {
      if (disposed || version !== generation || !node.isConnected) return;
      node.replaceChildren(el('span', 'training-help', '加载图片…'));
      try {
        let result = previews.get(key);
        if (!result) {
          result = await request('sources.preview', { importId: source.importId });
          if (!/^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(result.dataUrl || '')) throw new Error('图片预览格式无效。');
          if (disposed) return;
          previews.set(key, result); while (previews.size > 24) previews.delete(previews.keys().next().value);
        }
        if (version !== generation || !node.isConnected) return;
        const image = el('img', 'training-source-image'); image.src = result.dataUrl;
        image.alt = source.name || '训练图片'; image.decoding = 'async';
        image.title = `${result.originalWidth} × ${result.originalHeight}`;
        node.replaceChildren(image);
      } catch (error) {
        if (disposed || version !== generation || !node.isConnected) return;
        node.replaceChildren(el('small', 'training-field-error', error.message || '图片暂不可用'), control('重试预览', load));
      }
    }
    if (!source.importId || !request) { node.append(el('small', 'training-help', '保存数据集后预览')); return; }
    if (previews.has(key)) { queueMicrotask(load); return; }
    node._trainingPreview = () => { queue.push(load); pump(); };
    if (observer) observer.observe(node); else if (queue.length + running < 6) node._trainingPreview();
  }
  function pump() {
    while (!disposed && running < 2 && queue.length) {
      const load = queue.shift(); running++;
      Promise.resolve().then(load).finally(() => { running--; pump(); });
    }
  }
  function replaceSource(source, patch) {
    onChange(getSources().map(current => identity(current) === identity(source) ? { ...current, ...patch } : current));
  }
  function render() {
    if (disposed) return;
    observer?.disconnect(); queue = []; const version = ++generation;
    observer = typeof IntersectionObserver === 'function' ? new IntersectionObserver(entries => {
      for (const entry of entries) if (entry.isIntersecting) { observer?.unobserve(entry.target); entry.target._trainingPreview?.(); }
    }, { rootMargin: '100px' }) : null;
    const sources = getSources(), images = sources.filter(imageSource), summary = el('div', 'training-source-summary');
    const sourceKeys = new Set(sources.map(identity)); for (const key of selected) if (!sourceKeys.has(key)) selected.delete(key);
    host.replaceChildren();
    if (!sources.length) { host.append(el('p', 'training-empty', '尚未添加文件')); return; }
    summary.append(el('span', '', `${sources.length} 个文件${images.length ? ` · ${images.length} 张图片` : ''}`));
    host.append(summary);
    let updateSelection = () => {};
    {
      const batch = el('div', 'training-batch'), top = el('div', 'training-source-summary'), count = el('span');
      const chooseAll = control('全选', () => { sources.forEach(source => selected.add(identity(source))); render(); });
      const clear = control('取消选择', () => { selected.clear(); render(); });
      // 只从当前表单列表移除，不改动原文件和已保存的数据集。
      const removeSelected = control('移除所选', () => { onChange(getSources().filter(source => !selected.has(identity(source)))); selected.clear(); render(); });
      top.append(count, chooseAll, clear, removeSelected);
      const label = input('批量分类标签'), description = el('textarea'); description.rows = 2; description.placeholder = '批量图文描述';
      label.setAttribute('aria-label', '批量分类标签'); description.setAttribute('aria-label', '批量图文描述');
      const apply = patch => { onChange(getSources().map(source => selected.has(identity(source)) && imageSource(source) ? { ...source, ...patch(source) } : source)); render(); };
      const applyLabel = control('应用标签', () => { if (selected.size && label.value.trim()) apply(() => ({ label: label.value.trim() })); });
      const applyText = control('应用描述', () => { if (selected.size && description.value.trim()) apply(() => ({ text: description.value.trim() })); });
      const fromFolder = control('按父文件夹填标签', () => { if (selected.size) apply(source => !String(source.label || '').trim() && parentLabel(source) ? { label: parentLabel(source) } : {}); });
      updateSelection = () => {
        const chosen = images.filter(source => selected.has(identity(source)));
        count.textContent = `已选 ${selected.size} / ${sources.length} 个文件`;
        applyLabel.disabled = !chosen.length || !label.value.trim(); applyText.disabled = !chosen.length || !description.value.trim();
        fromFolder.disabled = !chosen.some(source => !String(source.label || '').trim() && parentLabel(source));
        clear.disabled = removeSelected.disabled = !selected.size; chooseAll.disabled = selected.size === sources.length;
      };
      label.oninput = description.oninput = updateSelection;
      const fields = el('div', 'training-source-fields'); fields.append(field('分类标签', label), field('图文描述', description));
      const commands = el('div', 'training-actions'); commands.append(applyLabel, applyText, fromFolder);
      batch.append(top); if (images.length) batch.append(fields, commands, el('small', 'training-help', '标签和描述仅修改勾选图片；按文件夹填充只补空标签。')); host.append(batch); updateSelection();
      const missing = images.filter(source => !String(source.label || '').trim()).length;
      const seen = new Set(); let duplicates = 0;
      for (const source of images) if (source.sha256) { if (seen.has(source.sha256)) duplicates++; else seen.add(source.sha256); }
      if (images.length) host.append(el('small', 'training-help', `${missing} 张未填分类标签${duplicates ? ` · ${duplicates} 张重复图片，保存时按样本去重` : ''}`));
    }
    const list = el('div', 'training-source-items');
    for (const source of sources) {
      const row = el('div', 'training-source'), header = el('div', 'training-source-header');
      const name = source.name || source.originalPath || source.path || '训练资料';
      const title = el('span', '', name); title.title = source.originalPath || source.path || name;
      const choice = el('input', 'training-source-check'); choice.type = 'checkbox'; choice.checked = selected.has(identity(source)); choice.setAttribute('aria-label', '选择 ' + name);
      choice.onchange = () => { if (choice.checked) selected.add(identity(source)); else selected.delete(identity(source)); updateSelection(); };
      header.append(choice);
      header.append(title, control('移除', () => { onChange(getSources().filter(current => identity(current) !== identity(source))); render(); }));
      row.append(header);
      if (imageSource(source)) {
        const thumbnail = el('div', 'training-source-thumbnail'); row.append(thumbnail);
        preview(thumbnail, source, version);
        const details = el('details', 'training-source-labels'), heading = el('summary');
        let labelValue = String(source.label ?? ''), textValue = String(source.text ?? '');
        const updateHeading = () => { heading.textContent = labelValue.trim() ? `标签：${labelValue.trim()}${textValue.trim() ? ' · 已填描述' : ''}` : textValue.trim() ? '已填图文描述' : '添加分类标签或图文描述'; };
        updateHeading(); details.append(heading);
        const label = input('例如：猫、狗', labelValue), description = el('textarea');
        label.setAttribute('aria-label', name + ' 分类标签');
        description.value = textValue; description.placeholder = '描述图中内容，供图文学习使用'; description.rows = 2;
        description.setAttribute('aria-label', name + ' 图文描述');
        label.oninput = () => { labelValue = label.value; replaceSource(source, { label: labelValue }); updateHeading(); };
        description.oninput = () => { textValue = description.value; replaceSource(source, { text: textValue }); updateHeading(); };
        const fields = el('div', 'training-source-fields'); fields.append(field('分类标签', label), field('图文描述', description)); details.append(fields); row.append(details);
      }
      list.append(row);
    }
    host.append(list);
  }
  return { render, dispose() { disposed = true; generation++; observer?.disconnect(); queue = []; previews.clear(); selected.clear(); } };
}
