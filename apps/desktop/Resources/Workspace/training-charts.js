import { element as el, button } from './preview-ui.js';
import { createSelectScope } from './workbench-controls.js';

const SVG = 'http://www.w3.org/2000/svg';
const skipped = new Set(['step', 'epoch', 'timestamp', 'time', 'elapsedSeconds', 'elapsedMs', 'totalSteps', 'samples', 'tokens']);
export const metricLabels = { loss: '训练损失', trainLoss: '训练损失', train_loss: '训练损失', evalLoss: '验证损失', eval_loss: '验证损失', validationLoss: '验证损失', learningRate: '学习率', learning_rate: '学习率', accuracy: '准确率', f1: 'F1', mae: 'MAE', mse: 'MSE', r2: 'R²', gpuMemoryMb: '显存 MiB', memoryMb: '内存 MiB', samplesPerSecond: '样本/秒' };
function finite(value) { return typeof value === 'number' && Number.isFinite(value); }
function tag(name, attrs = {}, text) { const n = document.createElementNS(SVG, name); for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, String(v)); if (text !== undefined) n.textContent = text; return n; }
export function metricKeys(rows) { return [...new Set(rows.flatMap(r => Object.keys(r).filter(k => !skipped.has(k) && finite(r[k]))))]; }
function points(rows, key) {
  const values = rows.flatMap((row, i) => finite(row[key]) ? [{ x: finite(row.step) ? row.step : i + 1, y: row[key] }] : []);
  if (values.some((point, i) => i > 0 && point.x < values[i - 1].x)) values.sort((a, b) => a.x - b.x);
  return values;
}
function compact(value) { return Math.abs(value) > 9999 || value !== 0 && Math.abs(value) < .001 ? value.toExponential(2) : Number(value.toPrecision(5)).toString(); }
function nearest(values, target) {
  let low = 0, high = values.length;
  while (low < high) { const middle = (low + high) >>> 1; if (values[middle].x < target) low = middle + 1; else high = middle; }
  if (!low) return 0; if (low === values.length) return low - 1;
  return target - values[low - 1].x <= values[low].x - target ? low - 1 : low;
}
// 屏幕抽点保留峰谷；导出数据与读数仍使用完整记录。
function screenPoints(values, limit = 1200) {
  if (values.length <= limit) return values;
  const result = [values[0]], bucket = Math.ceil((values.length - 2) / ((limit - 2) / 2));
  for (let start = 1; start < values.length - 1; start += bucket) {
    const end = Math.min(start + bucket, values.length - 1); let min = start, max = start;
    for (let i = start + 1; i < end; i++) { if (values[i].y < values[min].y) min = i; if (values[i].y > values[max].y) max = i; }
    result.push(values[Math.min(min, max)]); if (min !== max) result.push(values[Math.max(min, max)]);
  }
  result.push(values.at(-1)); return result;
}
function metricSvg(series, bounds, { width, height, smooth, label }) {
  const { xmin, xmax, ymin, ymax } = bounds, left = width < 400 ? 60 : 70, right = width - 18, top = 20, bottom = height - 48;
  const x = value => left + (value - xmin) / (xmax - xmin) * (right - left), y = value => bottom - (value - ymin) / (ymax - ymin) * (bottom - top);
  const svg = tag('svg', { xmlns: SVG, viewBox: `0 0 ${width} ${height}`, width, height, role: 'img', 'aria-label': label });
  svg.style.height = height + 'px'; svg.append(tag('rect', { width, height, fill: 'transparent' }));
  for (let i = 0; i <= 4; i++) {
    const yy = top + i * (bottom - top) / 4;
    svg.append(tag('line', { x1: left, x2: right, y1: yy, y2: yy, class: 'training-chart-grid' }), tag('text', { x: left - 8, y: yy + 4, 'text-anchor': 'end', class: 'training-chart-label' }, compact(ymax - i * (ymax - ymin) / 4)));
  }
  const intervals = width < 400 ? 2 : width < 600 ? 3 : 4, shown = new Set();
  for (let i = 0; i <= intervals; i++) {
    const value = Math.round(xmin + i * (xmax - xmin) / intervals); if (shown.has(value)) continue; shown.add(value);
    svg.append(tag('text', { x: x(value), y: bottom + 24, 'text-anchor': i === intervals ? 'end' : i === 0 ? 'start' : 'middle', class: 'training-chart-label' }, compact(value)));
  }
  svg.append(tag('text', { x: right, y: height - 5, 'text-anchor': 'end', class: 'training-chart-label' }, '训练步数'));
  series.forEach(s => {
    if (!s.points.length) return; let values = s.points;
    const line = (items, opacity = 1) => tag('polyline', { points: screenPoints(items).map(p => `${x(p.x)},${y(p.y)}`).join(' '), fill: 'none', stroke: s.color, opacity, 'stroke-width': 2, 'vector-effect': 'non-scaling-stroke' });
    if (smooth) { svg.append(line(values, .3)); let average = values[0].y; values = values.map(p => ({ ...p, y: average = average * .8 + p.y * .2 })); }
    svg.append(line(values)); if (values.length === 1) svg.append(tag('circle', { cx: x(values[0].x), cy: y(values[0].y), r: 3, fill: s.color }));
  });
  return { svg, x, left, right, top, bottom };
}
export function metricsCsv(rows) {
  const keys = [...new Set(rows.flatMap(r => Object.keys(r).filter(k => r[k] === null || ['string', 'number', 'boolean'].includes(typeof r[k]))))];
  const cell = value => '"' + (typeof value === 'string' && /^[=+\-@]/.test(value) ? "'" : '') + String(value ?? '').replaceAll('"', '""') + '"';
  return '\uFEFF' + [keys.map(cell).join(','), ...rows.map(r => keys.map(k => cell(r[k])).join(','))].join('\r\n');
}

export function createTrainingChart(parent, { rows = [], comparison = [], title = '训练指标', liveState = 'created', onExport, viewState = {}, onViewState } = {}) {
  const wrap = el('section', 'training-chart'), top = el('div', 'training-chart-toolbar'), plot = el('div', 'training-chart-plot');
  const legend = el('div', 'training-chart-legend'), tip = el('div', 'training-chart-tip'), live = el('span', 'training-chart-live'), empty = el('p', 'training-empty', '等待训练指标');
  const range = el('input', 'training-chart-range'); range.type = 'range'; range.min = '10'; range.max = '100'; range.value = '100'; range.setAttribute('aria-label', '显示最近指标范围，百分比');
  let keys = metricKeys(rows), selected = keys.includes('loss') ? 'loss' : keys[0], smooth = false, disposed = false, currentSvg, renderedCount = -1, renderedLast, renderedComparison, activePoints = [], cursorAt = -1, showPoint = null, hidePoint = null, renderedWidth = 0, resizeFrame = 0, renderedChart;
  let pendingRows = rows, pendingComparison = comparison, viewPaused = false;
  const marks = Array.isArray(viewState.marks) ? viewState.marks.filter(m => finite(m.step) && typeof m.label === 'string').slice(0, 100) : [];
  const from = el('input'), to = el('input'); from.type = to.type = 'number'; from.min = to.min = '0'; from.placeholder = '起始步'; to.placeholder = '结束步';
  from.value = viewState.from ?? ''; to.value = viewState.to ?? ''; from.setAttribute('aria-label', '图表起始步'); to.setAttribute('aria-label', '图表结束步');
  const persistView = () => onViewState?.({ from: from.value, to: to.value, marks: marks.map(m => ({ ...m })) });
  const tools = el('details', 'training-details training-chart-tools'); tools.append(el('summary', '', '缩放与标记'));
  const windowRow = el('div', 'training-actions workbench-row'), note = el('input'); note.placeholder = '标记说明（可选）'; note.maxLength = 80; note.setAttribute('aria-label', '曲线标记说明');
  const markList = el('div', 'training-chart-marks');
  function showMarks() { markList.replaceChildren(); for (const mark of marks) { const b = button(`第 ${mark.step} 步${mark.label ? ' · ' + mark.label : ''} ×`, () => { marks.splice(marks.indexOf(mark), 1); persistView(); showMarks(); draw(); }, '移除此标记'); markList.append(b); } }
  const pin = button('标记当前点', () => { const point = activePoints[cursorAt < 0 ? activePoints.length - 1 : cursorAt]; if (!point || marks.length >= 100) return; const existing = marks.find(m => m.step === point.x && m.metric === selected); if (existing) existing.label = note.value.trim(); else marks.push({ step: point.x, metric: selected, label: note.value.trim() }); note.value = ''; persistView(); showMarks(); draw(); });
  const viewToggle = button('暂停图表更新', () => { viewPaused = !viewPaused; if (viewPaused) { rows = rows.slice(); comparison = comparison.slice(); } else { rows = pendingRows; comparison = pendingComparison; keys = metricKeys(rows); updateKinds(); } viewToggle.textContent = viewPaused ? '恢复实时图表' : '暂停图表更新'; viewToggle.setAttribute('aria-pressed', String(viewPaused)); showLive(); draw(); });
  viewToggle.setAttribute('aria-pressed', 'false');
  windowRow.append(from, to, button('显示全部', () => { from.value = to.value = ''; range.value = '100'; persistView(); draw(); }), viewToggle);
  const markRow = el('div', 'training-actions workbench-row'); markRow.append(note, pin); tools.append(windowRow, markRow, markList); showMarks();
  from.oninput = to.oninput = () => { persistView(); draw(); };
  function appendMarks(svg, bounds, geometry) {
    for (const mark of marks.filter(m => m.metric === selected && m.step >= bounds.xmin && m.step <= bounds.xmax)) {
      const line = tag('line', { x1: geometry.x(mark.step), x2: geometry.x(mark.step), y1: geometry.top, y2: geometry.bottom, stroke: 'currentColor', opacity: '.45', 'stroke-dasharray': '3 5', class: 'training-chart-mark' });
      line.append(tag('title', {}, `第 ${mark.step} 步 ${mark.label}`)); svg.append(line);
    }
  }
  const choices = createSelectScope(wrap), kindHost = el('span', 'training-chart-kind'); let kind;
  function updateKinds() { choices.close(); kind = choices.select(keys.map(key => [key, metricLabels[key] || key]), selected || '', value => { selected = value; draw(); }); kind.setAttribute('aria-label', '图表指标'); kindHost.replaceChildren(kind); }
  const smoothing = button('原始曲线', () => { smooth = !smooth; smoothing.textContent = smooth ? '平滑 + 原始' : '原始曲线'; smoothing.setAttribute('aria-pressed', String(smooth)); draw(); }, '切换原始线与平滑线'); smoothing.setAttribute('aria-pressed', 'false');
  function exportSvg() {
    if (!currentSvg || !renderedChart) return null;
    const exported = metricSvg(renderedChart.series, renderedChart.bounds, { width: 900, height: 310, smooth, label: renderedChart.label }), source = exported.svg;
    appendMarks(source, renderedChart.bounds, exported);
    source.style.display = 'none'; plot.append(source); const copy = source.cloneNode(true), original = [source, ...source.querySelectorAll('*')], nodes = [copy, ...copy.querySelectorAll('*')];
    try { original.forEach((node, i) => { const style = getComputedStyle(node); for (const key of ['fill', 'stroke', 'font-family', 'font-size', 'opacity']) nodes[i].style.setProperty(key, style.getPropertyValue(key)); }); }
    finally { source.remove(); }
    copy.style.removeProperty('display'); copy.style.removeProperty('height');
    const background = getComputedStyle(plot).backgroundColor, text = getComputedStyle(wrap).color;
    copy.setAttribute('width', '900'); copy.setAttribute('height', '380'); copy.setAttribute('viewBox', '0 0 900 380'); copy.querySelector('.training-chart-cursor')?.remove();
    const drawing = tag('g', { transform: 'translate(0, 32)' }); while (copy.firstChild) drawing.append(copy.firstChild);
    copy.append(tag('rect', { width: 900, height: 380, fill: background }), drawing);
    copy.append(tag('text', { x: 20, y: 22, fill: text, 'font-size': 15, 'font-family': 'sans-serif' }, title + ' · ' + (metricLabels[selected] || selected)));
    copy.append(tag('text', { x: 70, y: 359, fill: text, 'font-size': 12, 'font-family': 'sans-serif' }, [...legend.children].map(n => n.textContent).join('  /  ')));
    copy.append(tag('text', { x: 70, y: 376, fill: text, 'font-size': 11, 'font-family': 'sans-serif' }, rangeLabel.textContent));
    return new XMLSerializer().serializeToString(copy);
  }
  async function exportPng() {
    const source = exportSvg(); if (!source) return; const url = URL.createObjectURL(new Blob([source], { type: 'image/svg+xml' }));
    try { const img = new Image(); img.src = url; await img.decode(); if (disposed) return; const canvas = document.createElement('canvas'); canvas.width = 1800; canvas.height = 760; canvas.getContext('2d').drawImage(img, 0, 0, canvas.width, canvas.height); await onExport?.('训练曲线.png', canvas.toDataURL('image/png').split(',')[1], 'image/png', 'base64'); }
    finally { URL.revokeObjectURL(url); }
  }
  const svgButton = button('SVG', () => { const source = exportSvg(); if (source) onExport?.('训练曲线.svg', source, 'image/svg+xml'); }, '导出SVG曲线'); svgButton.setAttribute('aria-label', '导出SVG曲线');
  const pngButton = button('PNG', () => void exportPng().catch(() => { tip.textContent = '暂时无法生成PNG，请导出SVG或CSV。'; tip.hidden = false; }), '导出PNG曲线'); pngButton.setAttribute('aria-label', '导出PNG曲线');
  const csvButton = button('CSV', () => onExport?.('训练指标.csv', metricsCsv(rows), 'text/csv'), '导出原始指标'); csvButton.setAttribute('aria-label', '导出原始指标');
  function showLive() {
    const last = rows.at(-1), timestamp = last?.timestamp ? new Date(last.timestamp) : null;
    const names = { disconnected: '连接中断', running: '实时更新', preparing: '准备中', queued: '等待资源', pausing: '保存中', paused: '已暂停', stopping: '停止中', stopped: '已停止', completed: '已完成', failed: '训练失败', interrupted: '已中断' };
    const time = timestamp && Number.isFinite(timestamp.valueOf()) ? timestamp.toLocaleTimeString('zh-CN', { hour12: false }) : '';
    live.textContent = (names[liveState] || '等待指标') + (liveState === 'running' && last?.step !== undefined ? ` · 第 ${last.step} 步` : time ? ` · ${time}` : ''); live.dataset.state = liveState;
    if (viewPaused) live.textContent = '图表已冻结 · 训练继续，指标仍在记录';
    live.title = time ? `最后记录：${time}${last?.step !== undefined ? ` · 第 ${last.step} 步` : ''}` : '';
    const emptyNames = { queued: '等待可用资源', preparing: '正在准备模型', running: '等待第一条指标', completed: '暂无曲线，请查看训练结果', failed: '暂无曲线，原因见上方', paused: '已暂停，尚无指标', stopped: '已停止，尚无指标', disconnected: '连接恢复后更新曲线' };
    empty.textContent = emptyNames[liveState] || '开始训练后显示曲线';
  }
  const exports = el('div', 'training-chart-exports'); exports.setAttribute('role', 'group'); exports.setAttribute('aria-label', '曲线导出'); exports.append(svgButton, pngButton, csvButton);
  top.append(live, kindHost, smoothing, exports); showLive();
  const rangeLabel = el('span', '', '全部记录'), rangeRow = el('label', 'training-chart-range-row'); rangeRow.append(el('span', '', '查看范围'), range, rangeLabel);
  plot.setAttribute('role', 'group'); plot.setAttribute('aria-label', '训练曲线；使用左右方向键查看真实指标，Home 和 End 跳到首尾'); tip.setAttribute('role', 'status'); tip.setAttribute('aria-live', 'off');
  plot.addEventListener('keydown', event => {
    if (!activePoints.length) return;
    if (event.key === 'Escape') { hidePoint?.(); return; }
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown'].includes(event.key)) return;
    event.preventDefault(); tip.setAttribute('aria-live', 'polite');
    const at = cursorAt < 0 ? activePoints.length - 1 : cursorAt;
    cursorAt = event.key === 'Home' ? 0 : event.key === 'End' ? activePoints.length - 1 : Math.max(0, Math.min(activePoints.length - 1, at + (event.key === 'ArrowLeft' ? -1 : event.key === 'ArrowRight' ? 1 : event.key === 'PageUp' ? -10 : 10)));
    showPoint?.(cursorAt);
  });
  plot.addEventListener('focus', () => { if (activePoints.length) { tip.setAttribute('aria-live', 'polite'); showPoint?.(cursorAt < 0 ? activePoints.length - 1 : cursorAt); } });
  plot.addEventListener('blur', () => hidePoint?.());
  plot.append(empty, tip); wrap.append(top, plot, legend, rangeRow, tools); parent.append(wrap);
  function draw() {
    if (disposed) return; const focusedStep = cursorAt >= 0 ? activePoints[cursorAt]?.x : undefined;
    renderedWidth = Math.max(180, Math.round(plot.clientWidth || parent.clientWidth || 600));
    renderedCount = rows.length; renderedLast = rows.at(-1); renderedComparison = comparison; currentSvg?.remove(); currentSvg = null; renderedChart = null; legend.replaceChildren(); tip.hidden = true; showPoint = hidePoint = null;
    const available = selected ? points(rows, selected) : [], previous = selected ? points(comparison, selected) : [];
    empty.hidden = !!available.length; svgButton.disabled = pngButton.disabled = smoothing.disabled = range.disabled = !available.length; csvButton.disabled = !rows.length;
    plot.tabIndex = available.length ? 0 : -1; activePoints = [];
    if (!available.length) return;
    const fraction = Number(range.value) / 100, custom = from.value !== '' || to.value !== '';
    const recent = custom ? available.filter(p => (from.value === '' || p.x >= Number(from.value)) && (to.value === '' || p.x <= Number(to.value))) : available.slice(Math.max(0, available.length - Math.max(2, Math.ceil(available.length * fraction))));
    pin.disabled = !recent.length || marks.length >= 100;
    if (!recent.length) { empty.hidden = false; empty.textContent = '此范围暂无指标'; svgButton.disabled = pngButton.disabled = true; return; }
    activePoints = recent; cursorAt = finite(focusedStep) ? nearest(recent, focusedStep) : -1;
    rangeLabel.textContent = custom ? `第 ${recent[0].x}–${recent.at(-1).x} 步 · ${recent.length} 条` : fraction === 1 ? `全部 ${available.length} 条` : `最近 ${range.value}% · ${recent.length} 条`; range.setAttribute('aria-valuetext', rangeLabel.textContent);
    const series = [{ label: metricLabels[selected] || selected, points: recent, color: 'var(--workbench-accent, #7799cc)' }];
    const validation = selected === 'loss' || selected === 'trainLoss' || selected === 'train_loss' ? keys.find(k => ['evalLoss', 'eval_loss', 'validationLoss'].includes(k)) : null;
    const withinRange = p => p.x >= recent[0].x && p.x <= recent.at(-1).x;
    if (validation) series.push({ label: '验证损失', points: points(rows, validation).filter(withinRange), color: 'var(--training-comparison, #a68dd1)' });
    if (previous.length) series.push({ label: '对照实验', points: previous.filter(withinRange), color: 'var(--appearance-secondary, #9ca3af)' });
    const all = series.flatMap(s => s.points), bounds = all.reduce((b, p) => ({ xmin: Math.min(b.xmin, p.x), xmax: Math.max(b.xmax, p.x), low: Math.min(b.low, p.y), high: Math.max(b.high, p.y) }), { xmin: Infinity, xmax: -Infinity, low: Infinity, high: -Infinity });
    const xmin = bounds.xmin, xmax = Math.max(xmin + 1, bounds.xmax), low = bounds.low, high = bounds.high, pad = Math.max(Math.abs(high) * .02, (high - low) * .08, .000001);
    const ymin = low >= 0 ? Math.max(0, low - pad) : low - pad, ymax = high + pad;
    const w = renderedWidth, h = w < 400 ? 240 : w < 700 ? 260 : 280, label = title + '，' + (metricLabels[selected] || selected);
    const geometry = metricSvg(series, { xmin, xmax, ymin, ymax }, { width: w, height: h, smooth, label }), { x, left, right, top: plotTop, bottom } = geometry;
    const svg = currentSvg = geometry.svg; renderedChart = { series, bounds: { xmin, xmax, ymin, ymax }, label };
    appendMarks(svg, renderedChart.bounds, geometry);
    series.forEach(s => {
      if (!s.points.length) return;
      const item = el('span'); item.style.setProperty('--series-color', s.color); item.append(el('i'), el('span', '', s.label)); legend.append(item);
    });
    if (smooth) legend.append(el('span', 'training-help', '浅线：原始 · 实线：平滑'));
    const cursor = tag('line', { y1: plotTop, y2: bottom, class: 'training-chart-cursor', visibility: 'hidden' }); svg.append(cursor);
    showPoint = index => {
      cursorAt = Math.max(0, Math.min(recent.length - 1, index)); const found = recent[cursorAt];
      cursor.setAttribute('x1', x(found.x)); cursor.setAttribute('x2', x(found.x)); cursor.setAttribute('visibility', 'visible');
      const readouts = series.flatMap(s => { if (!s.points.length) return []; const p = s.points[nearest(s.points, found.x)]; return p.x === found.x ? [`${s.label} ${compact(p.y)}`] : []; });
      tip.textContent = `步数 ${Number.isInteger(found.x) ? found.x : compact(found.x)} · ${readouts.join(' · ')}${smooth ? '（原始值）' : ''}`; tip.hidden = false;
    };
    hidePoint = () => { tip.hidden = true; cursor.setAttribute('visibility', 'hidden'); };
    svg.addEventListener('pointermove', event => {
      const bounds = svg.getBoundingClientRect(), xx = (event.clientX - bounds.left) / bounds.width * w, target = xmin + (xx - left) / (right - left) * (xmax - xmin);
      tip.setAttribute('aria-live', 'off'); showPoint(nearest(recent, target));
    });
    svg.addEventListener('pointerleave', () => { if (document.activeElement !== plot) hidePoint(); }); plot.prepend(svg);
    if (document.activeElement === plot) showPoint(cursorAt < 0 ? recent.length - 1 : cursorAt);
  }
  const resizeObserver = new ResizeObserver(() => {
    const width = Math.round(plot.clientWidth); if (disposed || !width || Math.abs(width - renderedWidth) < 1) return;
    cancelAnimationFrame(resizeFrame); resizeFrame = requestAnimationFrame(() => { resizeFrame = 0; draw(); });
  });
  resizeObserver.observe(plot);
  range.oninput = () => { from.value = to.value = ''; persistView(); draw(); }; updateKinds(); draw();
  return { update(nextRows, nextComparison = comparison) { pendingRows = nextRows; pendingComparison = nextComparison; if (viewPaused) return; const changedRows = nextRows.length !== renderedCount || nextRows.at(-1) !== renderedLast || nextComparison !== renderedComparison; rows = nextRows; comparison = nextComparison; showLive(); if (!changedRows) return; const nextKeys = metricKeys(rows), changed = JSON.stringify(nextKeys) !== JSON.stringify(keys); keys = nextKeys; if (!keys.includes(selected)) selected = keys.includes('loss') ? 'loss' : keys[0]; if (changed) updateKinds(); draw(); }, setStatus(value) { liveState = value; showLive(); }, dispose() { disposed = true; resizeObserver.disconnect(); cancelAnimationFrame(resizeFrame); choices.dispose(); wrap.remove(); } };
}
