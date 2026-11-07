import { element as el } from './preview-ui.js';

const splitNames = { train: '训练', validation: '验证', test: '测试' };
function display(value) { return value == null ? '—' : typeof value === 'object' ? JSON.stringify(value) : String(value); }
function sourceLabel(row) {
  const source = row.source || {}, base = source.path || source.name || (source.kind === 'inline' ? '粘贴内容' : '直接样本');
  return [base, source.sheetName, row.sourceRow ? `第 ${row.sourceRow} 行` : '', Number.isInteger(row.chunkIndex) ? `片段 ${row.chunkIndex + 1}` : ''].filter(Boolean).join(' · ');
}

export function createTrainingDatasetPreview(host, { request, control }) {
  let selected = null, disposed = false, sequence = 0, page = null, loading = false, offsets = [], thumbnails = new Map();
  async function imageFor(node, row, version) {
    const key = `${selected?.id}:${row.index}`;
    try {
      let result = thumbnails.get(key);
      if (!result) {
        result = await request('sources.preview', { datasetId: selected.id, sampleIndex: row.index });
        if (!/^data:image\/png;base64,[A-Za-z0-9+/]+=*$/.test(result.dataUrl || '')) throw new Error('图片预览格式无效。');
        if (disposed || version !== sequence) return;
        thumbnails.set(key, result); while (thumbnails.size > 24) thumbnails.delete(thumbnails.keys().next().value);
      }
      if (disposed || version !== sequence || !node.isConnected) return;
      const image = el('img', 'training-dataset-image'); image.src = result.dataUrl; image.alt = row.source?.name || '训练样本'; image.decoding = 'async';
      node.replaceChildren(image, el('small', 'training-help', `${result.originalWidth} × ${result.originalHeight}`));
    } catch (error) {
      if (disposed || version !== sequence || !node.isConnected) return;
      node.replaceChildren(el('small', 'training-field-error', error.message || '图片预览失败'), control('重试预览', () => imageFor(node, row, version)));
    }
  }
  function stats(data) {
    const box = el('div', 'training-dataset-stats'), s = data.statistics || {}, splits = s.splitCounts || {};
    for (const value of [`${data.total} 条样本`, `训练 ${splits.train || 0} · 验证 ${splits.validation || 0} · 测试 ${splits.test || 0}`,
      Number.isFinite(s.duplicatesRemoved) ? `已去重 ${s.duplicatesRemoved} 条` : '', s.emptyContent ? `空内容 ${s.emptyContent} 条` : '',
      s.missingFeatureCells ? `缺失特征 ${s.missingFeatureCells} 处` : '',
      data.dataset?.kind === 'image' ? `未填分类标签 ${s.missingLabels || 0} · 未填图文描述 ${s.missingDescriptions || 0}` : '']) {
      if (value) box.append(el('span', 'training-dataset-stat', value));
    }
    return box;
  }
  function table(data) {
    const scroll = el('div', 'training-dataset-table-scroll'), table = el('table', 'training-dataset-table'), head = el('thead'), line = el('tr');
    const featureCount = Math.max(...data.rows.map(row => row.features?.length || 0), 0), names = data.featureNames || [];
    const hasLabel = data.rows.some(row => Object.hasOwn(row, 'label'));
    const titles = ['序号', ...Array.from({ length: featureCount }, (_, i) => names[i] || `特征 ${i + 1}`), ...(hasLabel ? [data.targetName || '标签'] : []), '划分', '来源'];
    for (const title of titles) { const cell = el('th', '', title); cell.scope = 'col'; line.append(cell); }
    head.append(line); table.append(head); const body = el('tbody');
    for (const row of data.rows) {
      const tr = el('tr');
      const values = [row.index + 1, ...Array.from({ length: featureCount }, (_, i) => row.features?.[i]), ...(hasLabel ? [row.label] : []), splitNames[row.split] || row.split, sourceLabel(row)];
      values.forEach(value => { const td = el('td', '', display(value)); td.title = display(value); tr.append(td); }); body.append(tr);
    }
    table.append(body); scroll.append(table);
    if (data.featureNamesTruncated || data.rows.some(row => row.featuresTruncated)) scroll.append(el('small', 'training-help', '预览展示前 64 个特征，训练使用完整数据。'));
    return scroll;
  }
  function samples(data, version) {
    const list = el('div', 'training-dataset-samples');
    for (const row of data.rows) {
      const card = el('article', 'training-dataset-sample'), title = el('div', 'training-source-header');
      title.append(el('strong', '', `样本 ${row.index + 1}`), el('span', 'training-help', splitNames[row.split] || row.split || ''));
      card.append(title, el('small', 'training-dataset-source', sourceLabel(row)));
      if (row.hasImage) {
        const preview = el('details', 'training-dataset-image-preview'), summary = el('summary', '', '查看图片'), area = el('div', 'training-source-thumbnail');
        preview.append(summary, area); let loaded = false;
        preview.ontoggle = () => { if (preview.open && !loaded) { loaded = true; area.append(el('small', 'training-help', '加载图片…')); imageFor(area, row, version); } }; card.append(preview);
      }
      if (row.label != null) card.append(el('p', 'training-dataset-label', `标签：${display(row.label)}`));
      const content = row.text || [row.prompt, row.completion].filter(Boolean).join('\n\n') || (row.messages || []).map(message => `${message.role || '消息'}：${message.content || ''}`).join('\n\n');
      if (content) card.append(el('pre', 'training-dataset-text', content));
      if (!content && !row.hasImage) card.append(el('small', 'training-field-error', '这条样本没有文本内容。'));
      if (row.textTruncated || row.messagesTruncated) card.append(el('small', 'training-help', '预览已截取，训练仍使用完整样本。'));
      list.append(card);
    }
    return list;
  }
  function render(data, version) {
    host.replaceChildren(el('h3', '', `${data.dataset.name || selected.name || '数据集'} · 样本预览`), stats(data));
    host.append(el('small', 'training-help', '当前版本只读；修改来源后保存为新数据集。'));
    const warnings = data.dataset.warnings || []; if (warnings.length) {
      const details = el('details', 'training-dataset-warnings'); details.append(el('summary', '', `数据提示 ${warnings.length}`));
      warnings.forEach(warning => details.append(el('p', 'training-help', warning))); host.append(details);
    }
    if (!data.rows.length) host.append(el('p', 'training-empty', '没有可预览的样本。'));
    else host.append(data.dataset.kind === 'tabular' ? table(data) : samples(data, version));
    const pager = el('div', 'training-dataset-pager');
    const previous = control('上一页', () => { if (!loading && offsets.length) { const target = offsets.pop(); return load(target); } });
    const next = control('下一页', () => { if (!loading && Number.isInteger(page?.nextOffset)) { offsets.push(page.offset); return load(page.nextOffset); } });
    previous.disabled = !offsets.length; next.disabled = !Number.isInteger(data.nextOffset);
    pager.append(previous, el('span', 'training-help', `${data.rows.length ? data.offset + 1 : 0}–${data.offset + data.rows.length} / ${data.total}`), next); host.append(pager);
  }
  async function load(offset = 0) {
    if (disposed || !selected) return; const version = ++sequence, datasetId = selected.id; loading = true;
    host.setAttribute('aria-busy', 'true'); host.replaceChildren(el('p', 'training-help', '正在读取样本…'));
    try {
      const result = await request('datasets.read', { datasetId, offset, limit: 20 });
      if (disposed || version !== sequence) return; page = result; render(result, version);
    } catch (error) {
      if (disposed || version !== sequence) return;
      host.replaceChildren(el('small', 'training-field-error', error.message || '样本预览暂不可用'), control('重试', () => load(offset)));
    } finally { if (!disposed && version === sequence) { loading = false; host.removeAttribute('aria-busy'); } }
  }
  return {
    show(dataset) { if (disposed) return; selected = dataset; offsets = []; page = null; thumbnails.clear(); host.hidden = !dataset; if (!dataset) { sequence++; host.replaceChildren(); return; } return load(); },
    dispose() { disposed = true; sequence++; thumbnails.clear(); }
  };
}
