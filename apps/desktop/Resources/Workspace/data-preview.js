import { element, button, toolbar, field, select, input, disclosure, group } from './preview-ui.js';
import { createNotice } from './preview-notice.js';

export function csv(rows, delimiter = ',') {
  return rows.map(row => row.map(v => { const s = String(v ?? ''); return /["\r\n,\t]/.test(s) || s.includes(delimiter) ? '"' + s.replace(/"/g, '""') + '"' : s; }).join(delimiter)).join('\r\n');
}
export function numeric(value) { if (String(value ?? '').trim() === '') return null; const n = Number(String(value).replace(/,/g, '')); return Number.isFinite(n) ? n : null; }
export function summarize(rows, columns) {
  return columns.map((name, c) => {
    let missing = 0, count = 0, sum = 0, min = Infinity, max = -Infinity;
    for (const row of rows) { const value = row[c], n = numeric(value); if (!String(value ?? '').trim()) missing++;
      if (n !== null) { count++; sum += n; min = Math.min(min, n); max = Math.max(max, n); } }
    return { name, count: rows.length, missing, numeric: count, min: count ? min : null, max: count ? max : null, sum, average: count ? sum / count : null };
  });
}
export function pivot(rows, group, value, method) {
  const groups = new Map();
  for (const row of rows) { const key = String(row[group] ?? ''); if (!groups.has(key)) groups.set(key, { rows: 0, count: 0, sum: 0 });
    const item = groups.get(key), n = numeric(row[value]); item.rows++; if (n !== null) { item.count++; item.sum += n; } }
  return [...groups].map(([key, item]) => [key, method === 'count' ? item.rows : method === 'average' ? item.count ? item.sum / item.count : 0 : item.sum]);
}
function svgNode(tag, attributes = {}, text) { const node = document.createElementNS('http://www.w3.org/2000/svg', tag); for (const [k, v] of Object.entries(attributes)) node.setAttribute(k, v); if (text !== undefined) node.textContent = text; return node; }
export async function open(options) {
  const columns = (options.rows?.[0] || []).map((v, i) => String(v || '列 ' + (i + 1))), rows = (options.rows || []).slice(1).map(row => [...row]);
  const root = element('section'), tools = toolbar(), summaries = element('div', 'data-summary'), wrap = element('div', 'data-table-wrap'); tools.classList.add('data-controls');
  const search = input('筛选当前表格'), saved = options.state?.table || {}, edits = new Map(), rowIds = new Map(rows.map((row, i) => [row, i]));
  const sourceVersion = options.version ?? null, feedback = createNotice(root), editNote = element('span', 'preview-muted');
  let restoredEdits = [], warnedLargeState = false;
  if (saved.sourceVersion === sourceVersion && Array.isArray(saved.edits)) {
    for (const entry of saved.edits) { const [r, c, text] = Array.isArray(entry) ? entry : [];
      if (Number.isInteger(r) && Number.isInteger(c) && rows[r] && c >= 0 && c < columns.length && typeof text === 'string') { rows[r][c] = text; edits.set(`${r}:${c}`, [r, c, text]); } }
  } else if (saved.edits?.length) feedback.show('源文件已更新，已按新版本打开；旧版预览修改不会覆盖新内容。');
  let sortColumn = Number.isInteger(saved.sortColumn) && saved.sortColumn < columns.length ? saved.sortColumn : -1, ascending = saved.ascending !== false, page = Math.max(0, Number(saved.page) || 0), selectedX = Math.max(0, Math.min(columns.length - 1, saved.selectedX || 0)), selectedY = Math.max(0, Math.min(columns.length - 1, saved.selectedY ?? 1)), chartKind = ['bar','line','pie'].includes(saved.chartKind) ? saved.chartKind : 'bar', groupMode = ['none','sum','average','count'].includes(saved.groupMode) ? saved.groupMode : 'none', chartRows = [], filtered = [...rows];
  search.value = String(saved.filter || '').slice(0, 1000);
  const svg = svgNode('svg', { class: 'data-chart', viewBox: '0 0 760 260', role: 'img', 'aria-label': '数据图表' });
  const total = element('span', 'preview-muted'), pageText = element('span', 'preview-muted');
  total.setAttribute('role', 'status'); search.setAttribute('aria-label', '筛选当前表格');
  const choices = columns.map((v, i) => [String(i), v]);
  tools.append(search, total, button('导出当前结果', () => options.onExport?.({ filename: '筛选结果.csv', mimeType: 'text/csv', encoding: 'utf8', content: '\uFEFF' + csv([columns, ...filtered]) })),
    button('引用选中内容', () => { const text = window.getSelection()?.toString().trim(); if (text) options.onSelection?.({ kind: 'table', title: options.title, text: text.slice(0, 16000) }); }));
  function renderStats() {
    summaries.replaceChildren();
    for (const stat of summarize(filtered, columns)) { const d = element('div', 'data-stat'); d.append(element('strong', '', stat.name), element('div', '', `缺失 ${stat.missing}/${stat.count} · ${stat.numeric ? '数值 ' + stat.numeric : '文本'}`));
      if (stat.numeric) d.append(element('div', '', `最小 ${stat.min} · 最大 ${stat.max} · 均值 ${stat.average.toFixed(3)}`)); summaries.append(d); }
    editNote.textContent = edits.size ? `${edits.size} 处预览修改 · 导出后保存，原文件保留` : '可直接编辑单元格；导出保存结果，原文件保留';
    resetEdits.hidden = !edits.size;
  }
  const chartTools = toolbar(); chartTools.append(field('分类', select(choices, String(selectedX), v => { selectedX = Number(v); drawChart(); })),
    field('数值', select(choices, String(selectedY), v => { selectedY = Number(v); drawChart(); })),
    select([['bar', '柱状图'], ['line', '折线图'], ['pie', '饼图']], chartKind, v => { chartKind = v; drawChart(); }),
    select([['none', '逐行'], ['sum', '分组求和'], ['average', '分组均值'], ['count', '分组计数']], groupMode, v => { groupMode = v; drawChart(); }),
    button('导出图表 SVG', () => { const copy = svg.cloneNode(true); copy.removeAttribute('class'); copy.setAttribute('width', String(svg.viewBox.baseVal.width)); copy.setAttribute('height', '260'); options.onExport?.({ filename: '数据图表.svg', mimeType: 'image/svg+xml', encoding: 'utf8', content: new XMLSerializer().serializeToString(copy) }); }),
    button('导出汇总表', () => options.onExport?.({ filename: '汇总.csv', mimeType: 'text/csv', encoding: 'utf8', content: '\uFEFF' + csv([[columns[selectedX], columns[selectedY]], ...chartRows]) })));
  let editTimer = 0;
  function getState() {
    const currentEdits = [...edits.values()];
    if (JSON.stringify(currentEdits).length <= 800000) { restoredEdits = currentEdits; warnedLargeState = false; }
    else if (!warnedLargeState) { warnedLargeState = true; feedback.show('修改较多，请导出当前结果；超出预览缓存的部分不能随标签恢复。', true); }
    return { table: { filter: search.value, sortColumn, ascending, page, selectedX, selectedY, chartKind, groupMode, sourceVersion, edits: restoredEdits, statsOpen: statsPanel.open, chartOpen: chartPanel.open } };
  }
  function remember() { options.onState?.(getState()); }
  function apply(updateTable = true) {
    const term = search.value.toLocaleLowerCase(); filtered = rows.filter(row => row.some(value => String(value).toLocaleLowerCase().includes(term)));
    if (sortColumn >= 0) filtered.sort((a, b) => { const x = numeric(a[sortColumn]), y = numeric(b[sortColumn]); const result = x !== null && y !== null ? x - y : String(a[sortColumn] ?? '').localeCompare(String(b[sortColumn] ?? ''), undefined, { numeric: true }); return ascending ? result : -result; });
    page = Math.min(page, Math.max(0, Math.ceil(filtered.length / 200) - 1)); if (updateTable) renderTable(); renderStats(); drawChart();
  }
  function updateCell(cell, row, c) {
    const text = cell.textContent || '', r = rowIds.get(row); row[c] = text;
    if (text === String(options.rows?.[r + 1]?.[c] ?? '')) edits.delete(`${r}:${c}`); else edits.set(`${r}:${c}`, [r, c, text]);
    clearTimeout(editTimer); editTimer = setTimeout(() => { editTimer = 0; apply(false); remember(); }, 180);
  }
  function renderTable() {
    const table = element('table'), head = element('thead'), tr = element('tr'), body = element('tbody');
    columns.forEach((name, c) => { const cell = element('th', '', name + (sortColumn === c ? ascending ? ' ↑' : ' ↓' : '')); cell.tabIndex = 0; cell.scope = 'col';
      cell.setAttribute('aria-sort', sortColumn === c ? ascending ? 'ascending' : 'descending' : 'none');
      const sort = () => { ascending = sortColumn === c ? !ascending : true; sortColumn = c; apply(); remember(); wrap.querySelectorAll('th')[c]?.focus(); }; cell.onclick = sort; cell.onkeydown = e => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); sort(); } }; tr.append(cell); }); head.append(tr);
    filtered.slice(page * 200, (page + 1) * 200).forEach(row => { const tr = element('tr'); columns.forEach((_, c) => { const cell = element('td', '', row[c] ?? ''); cell.contentEditable = 'plaintext-only'; cell.setAttribute('aria-label', columns[c]);
      cell.oninput = () => updateCell(cell, row, c);
      cell.onblur = event => { updateCell(cell, row, c); clearTimeout(editTimer); editTimer = 0; apply(!wrap.contains(event.relatedTarget)); remember(); }; tr.append(cell); }); body.append(tr); }); table.append(head, body); wrap.replaceChildren(table);
    total.textContent = `${filtered.length} / ${rows.length} 行 · ${columns.length} 列${options.hasMore ? ' · 当前为已加载片段' : ''}`; pageText.textContent = `${page + 1} / ${Math.max(1, Math.ceil(filtered.length / 200))}`;
    previousPage.disabled = page <= 0; nextPage.disabled = page >= Math.ceil(filtered.length / 200) - 1;
    empty.hidden = filtered.length > 0;
  }
  function drawChart() {
    chartRows = groupMode === 'none' ? filtered.map(row => [String(row[selectedX] ?? ''), numeric(row[selectedY])]).filter(row => row[1] !== null) : pivot(filtered, selectedX, selectedY, groupMode);
    const values = chartRows.slice(0, 40), colors = ['#5c88dd', '#2b9d8f', '#ba86d4', '#dfab55', '#dd7a86'];
    const chartWidth = Math.max(280, Math.round(svg.clientWidth || 760)); svg.setAttribute('viewBox', `0 0 ${chartWidth} 260`); svg.replaceChildren();
    svg.append(svgNode('rect', { width: chartWidth, height: 260, fill: '#fff', rx: 12 }));
    const fullTitle = `${columns[selectedX] || ''} / ${columns[selectedY] || ''}${chartRows.length > 40 ? '（前 40 项）' : ''}`;
    const titleCharacters = [...fullTitle], titleLimit = Math.max(8, Math.floor((chartWidth - 40) / 13));
    const heading = svgNode('text', { class: 'chart-label', x: 20, y: 23, fill: '#333', 'font-size': 13, 'font-family': 'sans-serif' }, titleCharacters.length > titleLimit ? titleCharacters.slice(0, titleLimit - 1).join('') + '…' : fullTitle);
    heading.append(svgNode('title', {}, fullTitle)); svg.append(heading);
    if (!values.length) { svg.append(svgNode('text', { class: 'chart-label', x: 20, y: 100, fill: '#666' }, '选择有数值的列后显示图表')); return; }
    if (chartKind === 'pie') {
      const positive = values.filter(r => r[1] > 0), sum = positive.reduce((a, r) => a + r[1], 0); let start = -Math.PI / 2;
      const wide = chartWidth >= 560, legendX = wide ? 345 : 178, labelLength = Math.max(3, Math.floor((chartWidth - legendX - 48) / 12));
      positive.forEach((row, i) => { const angle = row[1] / sum * Math.PI * 2, end = start + angle, cx = wide ? 205 : 90, cy = 142, r = wide ? 91 : 74;
        if (angle >= Math.PI * 2 - 1e-8) svg.append(svgNode('circle', { cx, cy, r, fill: colors[i % colors.length] }));
        else svg.append(svgNode('path', { d: `M${cx},${cy} L${cx + r * Math.cos(start)},${cy + r * Math.sin(start)} A${r},${r} 0 ${angle > Math.PI ? 1 : 0},1 ${cx + r * Math.cos(end)},${cy + r * Math.sin(end)} Z`, fill: colors[i % colors.length] }));
        if (i < 10) svg.append(svgNode('text', { x: legendX, y: 50 + i * 19, fill: colors[i % colors.length], 'font-size': 12 }, `${row[0].slice(0, labelLength)} ${(row[1] / sum * 100).toFixed(1)}%`)); start = end; });
    } else {
      const min = Math.min(0, ...values.map(r => r[1])), max = Math.max(0, ...values.map(r => r[1])), span = max - min || 1;
      const plotWidth = chartWidth - 75, labelCount = Math.max(3, Math.floor(plotWidth / 68));
      const py = v => 217 - (v - min) / span * 165, step = plotWidth / values.length, zero = py(0), points = [];
      svg.append(svgNode('line', { x1: 55, y1: zero, x2: chartWidth - 20, y2: zero, stroke: '#aaa' }));
      values.forEach((row, i) => { const x = 55 + step * i + step / 2, y = py(row[1]); points.push(`${x},${y}`);
        if (chartKind === 'bar') svg.append(svgNode('rect', { x: x - step * .35, y: Math.min(zero, y), width: Math.max(1, step * .7), height: Math.max(1, Math.abs(zero - y)), rx: 3, fill: colors[i % colors.length] }));
        else svg.append(svgNode('circle', { cx: x, cy: y, r: 3, fill: colors[0] }));
        if (i % Math.max(1, Math.ceil(values.length / labelCount)) === 0) svg.append(svgNode('text', { class: 'chart-label', x, y: 241, fill: '#555', 'text-anchor': 'middle', 'font-size': 11 }, row[0].slice(0, Math.max(3, Math.floor(plotWidth / labelCount / 12))))); });
      if (chartKind === 'line') svg.append(svgNode('polyline', { points: points.join(' '), fill: 'none', stroke: colors[0], 'stroke-width': 2 }));
      svg.append(svgNode('text', { class: 'chart-label', x: 8, y: 55, fill: '#555', 'font-size': 11 }, max.toFixed(1)), svgNode('text', { class: 'chart-label', x: 8, y: 217, fill: '#555', 'font-size': 11 }, min.toFixed(1)));
    }
  }
  search.addEventListener('input', () => { page = 0; apply(); remember(); });
  const pager = toolbar('表格翻页'), previousPage = button('上一页', () => { page = Math.max(0, page - 1); renderTable(); remember(); }), nextPage = button('下一页', () => { page = Math.min(Math.max(0, Math.ceil(filtered.length / 200) - 1), page + 1); renderTable(); remember(); }); pager.append(previousPage, pageText, nextPage);
  const empty = element('div', 'data-empty', '当前没有匹配行，清除或调整筛选后再试。');
  const statsPanel = disclosure('列统计 · 当前筛选', summaries), chartPanel = disclosure('图表与汇总', chartTools, svg);
  const resetEdits = button('还原预览修改', () => {
    clearTimeout(editTimer); editTimer = 0;
    for (const [r, c] of edits.values()) rows[r][c] = options.rows?.[r + 1]?.[c] ?? '';
    edits.clear(); apply(); remember();
  });
  statsPanel.classList.add('data-analysis'); chartPanel.classList.add('data-analysis'); statsPanel.open = saved.statsOpen === true; chartPanel.open = saved.chartOpen === true;
  chartPanel.addEventListener('toggle', () => { if (chartPanel.open) drawChart(); });
  root.append(tools, group('预览修改状态', editNote, resetEdits), wrap, empty, pager, statsPanel, chartPanel, feedback.node); options.container.append(root); apply();
  const observer = new ResizeObserver(drawChart); observer.observe(svg);
  return { getState, dispose() { clearTimeout(editTimer); observer.disconnect(); feedback.dispose(); }, setActive() {} };
}
