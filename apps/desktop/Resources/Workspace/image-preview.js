import { element, button, toolbar, field, group, disclosure, alive } from './preview-ui.js';
import { createNotice } from './preview-notice.js';

// Edits are non-destructive until explicitly exported. / 编辑结果显式导出，保留原图。
export async function open(options) {
  const { container } = options; const tools = toolbar(), stage = element('div', 'image-stage'), details = element('div', 'image-details');
  const image = new Image(); image.alt = options.title || '图片'; image.draggable = false;
  let disposed = false, width = 0, height = 0, scale = 1, x = 0, y = 0, rotation = 0, mode = 'pan', drag, crop, comparison, comparisonUrl, compareAmount = 50;
  let sourceUrl = null, note = '', annotations = [], fitted = true, lastWidth = 0, lastHeight = 0, pendingComparison = null;
  const urls = new Set(), swatch = element('span', 'preview-muted'), zoomText = element('span', 'preview-muted');
  const signal = options.signal, read = new AbortController(), cancel = () => read.abort(); signal?.addEventListener('abort', cancel, { once: true });
  const response = await fetch(options.contentUrl, { signal: read.signal, redirect: 'error' });
  if (!response.ok) throw new Error('图片读取失败：' + response.status);
  if (Number(response.headers.get('content-length')) > 32 * 1024 * 1024) throw new Error('图片超过 32 MiB 预览上限');
  const blob = await response.blob(); alive(options); if (blob.size > 32 * 1024 * 1024) throw new Error('图片超过 32 MiB 预览上限'); sourceUrl = URL.createObjectURL(blob); urls.add(sourceUrl);
  await new Promise((resolve, reject) => {
    const cleanup = () => signal?.removeEventListener('abort', cancelled);
    const cancelled = () => { cleanup(); image.src = ''; urls.forEach(URL.revokeObjectURL); reject(new DOMException('预览已切换', 'AbortError')); };
    image.onload = () => { cleanup(); resolve(); };
    image.onerror = () => { cleanup(); urls.forEach(URL.revokeObjectURL); reject(new Error('图片格式无法解码')); };
    signal?.addEventListener('abort', cancelled, { once: true });
    if (signal?.aborted) cancelled(); else image.src = sourceUrl;
  });
  alive(options); width = image.naturalWidth; height = image.naturalHeight; if (width * height > 64_000_000) { urls.forEach(URL.revokeObjectURL); throw new Error('图片超过 6400 万像素预览上限'); } stage.append(image); container.append(tools, stage, details);
  const feedback = createNotice(container); tools.after(feedback.node);
  stage.tabIndex = 0; stage.setAttribute('aria-label', '图片预览；加减号缩放，0适应窗口，R旋转，Esc清除裁剪');
  const cropBox = element('div', 'image-crop'); cropBox.hidden = true; stage.append(cropBox);
  const compareLayer = element('div', 'image-compare'); stage.append(compareLayer);
  const getState = () => ({ image: { scale, x, y, rotation, compareAmount, annotations, crop, autoFit: fitted, viewWidth: stage.clientWidth, viewHeight: stage.clientHeight } });
  function emit() { if (!disposed) options.onState?.(getState()); }
  function dimensions() { return rotation % 180 ? [height, width] : [width, height]; }
  function paint() {
    if (disposed) return;
    const transform = `translate(${x}px,${y}px) scale(${scale}) rotate(${rotation}deg)`;
    image.style.transform = transform;
    if (comparison) { comparison.style.transform = transform; compareLayer.style.clipPath = `inset(0 ${100 - compareAmount}% 0 0)`; }
    const radians = rotation * Math.PI / 180;
    for (const tag of stage.querySelectorAll('.image-annotation')) { const a = annotations[Number(tag.dataset.index)]; if (a) { tag.style.left = (x + (a.x * Math.cos(radians) - a.y * Math.sin(radians)) * scale) + 'px'; tag.style.top = (y + (a.x * Math.sin(radians) + a.y * Math.cos(radians)) * scale) + 'px'; } }
    cropBox.hidden = !crop || crop.width < 1 || crop.height < 1;
    if (!cropBox.hidden) {
      const matrix = new DOMMatrix(transform), corners = [[crop.x, crop.y], [crop.x + crop.width, crop.y], [crop.x, crop.y + crop.height], [crop.x + crop.width, crop.y + crop.height]].map(([px, py]) => new DOMPoint(px, py).matrixTransform(matrix));
      const left = Math.min(...corners.map(p => p.x)), top = Math.min(...corners.map(p => p.y));
      Object.assign(cropBox.style, { left: left + 'px', top: top + 'px', width: Math.max(...corners.map(p => p.x)) - left + 'px', height: Math.max(...corners.map(p => p.y)) - top + 'px' });
    }
    clearCrop.disabled = !crop;
    zoomText.textContent = Math.round(scale * 100) + '%';
  }
  function fit() { fitted = true; const [w, h] = dimensions(); scale = Math.max(.001, Math.min((stage.clientWidth - 24) / w, (stage.clientHeight - 24) / h, 4)); center(); }
  function center() { const [w, h] = dimensions(); x = (stage.clientWidth - w * scale) / 2; y = (stage.clientHeight - h * scale) / 2;
    if (rotation === 90) x += height * scale; else if (rotation === 180) { x += width * scale; y += height * scale; } else if (rotation === 270) y += width * scale;
    paint(); emit(); }
  function zoom(factor, px = stage.clientWidth / 2, py = stage.clientHeight / 2) {
    fitted = false;
    const next = Math.min(32, Math.max(.01, scale * factor)), ratio = next / scale;
    x = px - (px - x) * ratio; y = py - (py - y) * ratio; scale = next; paint(); emit();
  }
  function setMode(value) { mode = value; stage.classList.toggle('cropping', mode !== 'pan'); cropButton.setAttribute('aria-pressed', String(mode === 'crop')); }
  function rotate() { rotation = (rotation + 90) % 360; fit(); }
  const cropButton = button('裁剪', () => setMode(mode === 'crop' ? 'pan' : 'crop'));
  cropButton.setAttribute('aria-pressed', 'false');
  const clearCrop = button('清除裁剪', () => { crop = null; setMode('pan'); paint(); emit(); });
  const labelInput = element('input', 'preview-input'); labelInput.placeholder = '标注文字'; labelInput.setAttribute('aria-label', '标注文字');
  tools.append(group('查看图片', button('适应窗口', fit), button('原始尺寸', () => { fitted = false; scale = 1; center(); }), button('−', () => zoom(.8), '缩小'), zoomText,
    button('+', () => zoom(1.25), '放大'), button('旋转', rotate)));
  const edits = disclosure('编辑与标注', cropButton, clearCrop,
    button('引用区域', () => { const area = crop && crop.width >= 1 && crop.height >= 1 ? crop : { x: 0, y: 0, width, height }; options.onSelection?.({ kind: 'image', title: options.title, text: `原图 ${width} × ${height} 像素；选中区域（原图像素坐标）：${JSON.stringify(Object.fromEntries(Object.entries(area).map(([k, v]) => [k, Math.round(v)])))}${annotations.length ? '\n标注：' + annotations.map(a => a.text).join('；') : ''}` }); }),
    labelInput, button('加标注', () => { note = labelInput.value.trim(); if (note) { setMode('annotate'); feedback.show('在图片上点击放置标注，Esc取消。'); } }),
    button('清除标注', () => { annotations = []; for (const n of stage.querySelectorAll('.image-annotation')) n.remove(); emit(); }));
  const picker = element('input'); picker.type = 'file'; picker.accept = 'image/*'; picker.hidden = true;
  function releaseUrl(url) { if (url) { URL.revokeObjectURL(url); urls.delete(url); } }
  function cancelComparison() {
    if (!pendingComparison) return;
    pendingComparison.image.onload = pendingComparison.image.onerror = null; pendingComparison.image.src = ''; releaseUrl(pendingComparison.url); pendingComparison = null;
  }
  picker.addEventListener('change', () => {
    const file = picker.files?.[0]; picker.value = ''; if (!file || disposed) return;
    if (file.size > 32 * 1024 * 1024) { feedback.show('对比图片最多32 MiB，请缩小后再试。', true); return; }
    cancelComparison();
    const candidate = { image: new Image(), url: URL.createObjectURL(file) }; pendingComparison = candidate; urls.add(candidate.url);
    candidate.image.alt = '比较图片';
    candidate.image.onload = () => {
      if (disposed || pendingComparison !== candidate) return;
      if (candidate.image.naturalWidth * candidate.image.naturalHeight > 64_000_000) { cancelComparison(); feedback.show('对比图片超过6400万像素，请缩小后再试。', true); return; }
      releaseUrl(comparisonUrl); if (comparison) comparison.src = '';
      comparison = candidate.image; comparisonUrl = candidate.url; pendingComparison = null;
      comparison.width = width; comparison.height = height; compareLayer.replaceChildren(comparison); paint(); feedback.show('拖动对比分界查看两张图片。');
    };
    candidate.image.onerror = () => { if (!disposed && pendingComparison === candidate) { cancelComparison(); feedback.show('对比图片无法解码，请选择有效图片。', true); } };
    candidate.image.src = candidate.url;
  });
  const slider = element('input'); slider.type = 'range'; slider.min = 0; slider.max = 100; slider.value = compareAmount;
  slider.setAttribute('aria-label', '比较分界'); slider.addEventListener('input', () => { compareAmount = Number(slider.value); paint(); emit(); });
  const compare = disclosure('图片对比', button('选择对比图', () => picker.click()), picker, field('对比', slider), button('结束对比', () => { cancelComparison(); if (comparison) comparison.src = ''; comparison = null; releaseUrl(comparisonUrl); comparisonUrl = null; compareLayer.replaceChildren(); }));
  const exportPng = button('导出 PNG', () => exportImage('image/png')); exportPng.classList.add('primary');
  tools.append(exportPng, edits, compare, button('导出 JPEG', () => exportImage('image/jpeg')));
  const sizeText = element('span', '', `${width} × ${height} · ${(blob.size / 1024 / 1024).toFixed(2)} MiB`); details.append(sizeText, swatch, element('span', '', '滚轮缩放 · 拖动平移 · 双击适配'));
  function coordinates(event) {
    const rect = stage.getBoundingClientRect(), matrix = new DOMMatrix(getComputedStyle(image).transform).inverse();
    const p = new DOMPoint(event.clientX - rect.left - stage.clientLeft, event.clientY - rect.top - stage.clientTop).matrixTransform(matrix);
    return { x: Math.max(0, Math.min(width, p.x)), y: Math.max(0, Math.min(height, p.y)) };
  }
  function mark(a) { const tag = element('span', 'image-annotation', a.text); tag.dataset.index = annotations.indexOf(a); Object.assign(tag.style, { position: 'absolute', padding: '2px 6px', borderRadius: '6px', background: '#111c', color: '#fff', pointerEvents: 'none' }); stage.append(tag); paint(); }
  stage.addEventListener('pointerdown', event => {
    if (event.button !== 0) return; stage.setPointerCapture(event.pointerId);
    stage.focus({ preventScroll: true });
    const point = coordinates(event);
    if (mode === 'annotate') { const a = { x: point.x, y: point.y, text: note }; annotations.push(a); mark(a); setMode('pan'); feedback.hide(); emit(); return; }
    drag = { cx: event.clientX, cy: event.clientY, x, y, point };
    if (mode === 'crop') { crop = { x: point.x, y: point.y, width: 0, height: 0 }; paint(); }
  });
  stage.addEventListener('pointermove', event => {
    const point = coordinates(event); swatch.textContent = `${Math.round(point.x)}, ${Math.round(point.y)}`;
    if (!drag) return;
    if (mode === 'crop') {
      crop = { x: Math.min(point.x, drag.point.x), y: Math.min(point.y, drag.point.y), width: Math.abs(point.x - drag.point.x), height: Math.abs(point.y - drag.point.y) };
    } else { fitted = false; x = drag.x + event.clientX - drag.cx; y = drag.y + event.clientY - drag.cy; }
    paint();
  });
  stage.addEventListener('pointerup', () => { drag = null; emit(); }); stage.addEventListener('pointercancel', () => { drag = null; });
  stage.addEventListener('wheel', event => { event.preventDefault(); const r = stage.getBoundingClientRect(); zoom(Math.exp(-event.deltaY * .0015), event.clientX - r.left, event.clientY - r.top); }, { passive: false });
  stage.addEventListener('dblclick', fit);
  stage.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.key === '+' || event.key === '=') zoom(1.25);
    else if (event.key === '-') zoom(.8);
    else if (event.key === '0') fit();
    else if (event.key.toLowerCase() === 'r') rotate();
    else if (event.key === 'Escape') { drag = null; crop = null; setMode('pan'); feedback.hide(); paint(); emit(); }
    else return;
    event.preventDefault();
  });
  stage.addEventListener('contextmenu', event => {
    event.preventDefault(); const point = coordinates(event), canvas = document.createElement('canvas'); canvas.width = canvas.height = 1;
    const ctx = canvas.getContext('2d', { willReadFrequently: true }); ctx.drawImage(image, Math.min(width - 1, Math.floor(point.x)), Math.min(height - 1, Math.floor(point.y)), 1, 1, 0, 0, 1, 1);
    const rgba = ctx.getImageData(0, 0, 1, 1).data; const hex = '#' + [...rgba].slice(0, 3).map(v => v.toString(16).padStart(2, '0')).join('');
    swatch.textContent = `${hex} · α ${rgba[3]}`; swatch.style.borderBottom = '4px solid ' + hex;
  });
  function exportImage(mimeType) {
    try {
    const area = crop && crop.width >= 1 && crop.height >= 1 ? crop : { x: 0, y: 0, width, height };
    const canvas = document.createElement('canvas'), swapped = rotation % 180 !== 0;
    canvas.width = Math.round(swapped ? area.height : area.width); canvas.height = Math.round(swapped ? area.width : area.height);
    const ctx = canvas.getContext('2d'); if (mimeType === 'image/jpeg') { ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, canvas.width, canvas.height); }
    ctx.translate(canvas.width / 2, canvas.height / 2); ctx.rotate(rotation * Math.PI / 180);
    ctx.drawImage(image, area.x, area.y, area.width, area.height, -area.width / 2, -area.height / 2, area.width, area.height);
    ctx.font = Math.max(16, area.width / 60) + 'px sans-serif'; ctx.textBaseline = 'top';
    for (const a of annotations) { const px = a.x - area.x - area.width / 2, py = a.y - area.y - area.height / 2; ctx.lineWidth = 3; ctx.strokeStyle = '#111'; ctx.strokeText(a.text, px, py); ctx.fillStyle = '#fff'; ctx.fillText(a.text, px, py); }
    options.onExport?.({ filename: (options.title || '图片').replace(/\.[^.]+$/, '') + '-编辑.' + (mimeType === 'image/png' ? 'png' : 'jpg'), mimeType, encoding: 'base64', content: canvas.toDataURL(mimeType, .93).split(',')[1] });
    } catch { feedback.show('图片导出失败，请减小裁剪区域后重试。', true); }
  }
  const saved = options.state?.image;
  if (saved && Number.isFinite(saved.scale) && saved.scale > 0 && [saved.x, saved.y].every(Number.isFinite)) {
    ({ scale, x, y } = saved); rotation = [0, 90, 180, 270].includes(saved.rotation) ? saved.rotation : 0; annotations = Array.isArray(saved.annotations) ? saved.annotations.slice(0, 100) : [];
    fitted = saved.autoFit === true; compareAmount = Math.max(0, Math.min(100, Number.isFinite(saved.compareAmount) ? saved.compareAmount : 50)); slider.value = compareAmount;
    lastWidth = Number.isFinite(saved.viewWidth) && saved.viewWidth > 0 ? saved.viewWidth : stage.clientWidth;
    lastHeight = Number.isFinite(saved.viewHeight) && saved.viewHeight > 0 ? saved.viewHeight : stage.clientHeight;
    if (saved.crop && ['x', 'y', 'width', 'height'].every(name => Number.isFinite(saved.crop[name])) && saved.crop.width > 0 && saved.crop.height > 0 && saved.crop.x >= 0 && saved.crop.y >= 0 && saved.crop.x + saved.crop.width <= width && saved.crop.y + saved.crop.height <= height) crop = { ...saved.crop };
    for (const a of annotations) if (typeof a.text === 'string') mark(a); paint();
  } else fit();
  const observer = new ResizeObserver(() => {
    if (disposed) return;
    const nextWidth = stage.clientWidth, nextHeight = stage.clientHeight;
    if (lastWidth === nextWidth && lastHeight === nextHeight) return;
    if (fitted) fit();
    else { if (lastWidth) { x += (nextWidth - lastWidth) / 2; y += (nextHeight - lastHeight) / 2; } paint(); emit(); }
    lastWidth = nextWidth; lastHeight = nextHeight;
  }); observer.observe(stage);
  return { getState, setActive() {}, dispose() { disposed = true; read.abort(); signal?.removeEventListener('abort', cancel); observer.disconnect(); cancelComparison(); image.src = ''; comparison && (comparison.src = ''); urls.forEach(url => URL.revokeObjectURL(url)); feedback.dispose(); } };
}
