/* Offline, gateway-authorized model preview. No model text is inserted as HTML. */
import * as THREE from './vendor/three-0.180.0/build/three.module.min.js';
import { OrbitControls } from './vendor/three-0.180.0/addons/controls/OrbitControls.js';
import { GLTFLoader } from './vendor/three-0.180.0/addons/loaders/GLTFLoader.js';
import { OBJLoader } from './vendor/three-0.180.0/addons/loaders/OBJLoader.js';
import { STLLoader } from './vendor/three-0.180.0/addons/loaders/STLLoader.js';
import { MTLLoader } from './vendor/three-0.180.0/addons/loaders/MTLLoader.js';
import { RoomEnvironment } from './vendor/three-0.180.0/addons/environments/RoomEnvironment.js';
import { createNotice } from './preview-notice.js';

const STYLE_ID = 'kynxa-model-preview-style';
const BACKGROUNDS = { auto: null, light: '#e9edf4', dark: '#171e2a', gray: '#777c83', white: '#ffffff' };
const MAX_BYTES = 256 * 1024 * 1024;
const num = (v, d = 0) => Number.isFinite(Number(v)) ? Number(v) : d;
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const validVector = v => Array.isArray(v) && v.length === 3 && v.every(Number.isFinite);
const abortError = () => new DOMException('预览已取消', 'AbortError');
function element(tag, cls, text) { const el = document.createElement(tag); if (cls) el.className = cls; if (text != null) el.textContent = text; return el; }
function readState(value) { try { const s = typeof value === 'string' ? JSON.parse(value) : value; return s && typeof s === 'object' ? s : {}; } catch { return {}; } }
function canonical(value) { try { return decodeURIComponent(String(value)).replaceAll('\\', '/').replace(/^\.\//, ''); } catch { return String(value); } }

/**
 * @param {{container:HTMLElement,contentUrl:string,title?:string,state?:object|string,
 * signal?:AbortSignal,onState?:(state:object)=>void,isCurrent?:()=>boolean,
 * resources?:Object|Array,sourceFormat?:string,renderFormat?:string}} options
 * @returns {Promise<{getState:Function,dispose:Function,setActive:Function}>}
 */
export async function open(options) {
  const { container, contentUrl, signal, onState, isCurrent = () => true } = options;
  if (!container || !contentUrl) throw new Error('3D 预览缺少容器或授权内容地址。');
  if (signal?.aborted || !isCurrent()) throw abortError();
  if (!document.getElementById(STYLE_ID)) {
    const link = element('link'); link.id = STYLE_ID; link.rel = 'stylesheet'; link.href = new URL('./model-preview.css', import.meta.url).href; document.head.append(link);
  }
  const saved = readState(options.state), lifetime = new AbortController();
  const listen = (el, type, callback, extra = {}) => el.addEventListener(type, callback, { ...extra, signal: lifetime.signal });
  let disposed = false, active = true, contextLost = false, renderFailed = false, raf = 0, stateTimer = 0, lastFrame = 0, model = null, mixer = null, action = null;
  let selected = null, isolated = null, renderer = null, controls = null, grid = null, selectionBox = null, envTarget = null;
  let duration = 0, clipIndex = 0, playing = !!saved.playing, cameraReady = false, snapshot = null, progressSavedAt = 0, filterTimer = 0;
  const objectById = new Map(), idByObject = new Map(), rowById = new Map(), hidden = new Set(Array.isArray(saved.hidden) ? saved.hidden.map(String) : []);
  const materials = new Set(), geometries = new Set(), textures = new Set(), objectUrls = new Set();
  const settings = { wireframe: !!saved.wireframe, grid: saved.grid !== false, background: Object.hasOwn(BACKGROUNDS, saved.background) ? saved.background : 'auto', exposure: clamp(num(saved.exposure, 1), .2, 3) };
  const root = element('section', 'kx-model'); root.setAttribute('aria-label', '3D 模型预览');
  const top = element('div', 'kx-model-topbar'), title = element('div', 'kx-model-title', options.title || '3D 模型');
  title.title = options.title || '3D 模型';
  const badge = element('span', 'kx-model-badge', '3D · PBR'); top.append(element('span', 'kx-model-file-icon', '◇'), title, badge);
  const toolbar = element('div', 'kx-model-toolbar'); toolbar.setAttribute('role', 'toolbar'); toolbar.setAttribute('aria-label', '模型视图与显示设置');
  const primaryTools = element('div', 'kx-model-primary-tools'), viewTools = element('div', 'kx-model-view-tools'), displayTools = element('div', 'kx-model-display-tools');
  viewTools.hidden = true; displayTools.hidden = true; toolbar.append(primaryTools, viewTools, displayTools);
  const body = element('div', 'kx-model-body'), stage = element('div', 'kx-model-stage'); stage.tabIndex = 0;
  const status = element('div', 'kx-model-status', '正在加载模型…'); status.setAttribute('role', 'status');
  const side = element('aside', 'kx-model-side'), selectionInfo = element('div', 'kx-model-selection'), tree = element('div', 'kx-model-tree');
  tree.setAttribute('role', 'tree'); tree.setAttribute('aria-label', '模型对象树');
  const objectActions = element('div', 'kx-model-object-actions');
  const sideHeader = element('div', 'kx-model-side-header'); sideHeader.append(element('h3', 'kx-model-side-title', '部件与尺寸'));
  const searchBox = element('div', 'kx-model-search'), search = element('input'), treeCount = element('span', 'kx-model-tree-count');
  search.type = 'search'; search.placeholder = '查找部件名称或类型'; search.maxLength = 200; search.setAttribute('aria-label', '筛选部件'); search.value = String(saved.treeFilter || '').slice(0, 200);
  treeCount.setAttribute('role', 'status'); treeCount.setAttribute('aria-live', 'polite'); searchBox.append(search, treeCount);
  side.append(sideHeader, selectionInfo, searchBox, tree, objectActions);
  stage.append(status); body.append(stage, side);
  const notice = createNotice(stage, 'kx-model-notice');
  const animation = element('div', 'kx-model-animation'); animation.hidden = true;
  const footer = element('div', 'kx-model-footer'), summary = element('span', '', '准备渲染'); summary.setAttribute('role', 'status');
  const gestureHint = element('details', 'kx-model-help'), helpTitle = element('summary', '', '操作说明'), helpBody = element('div');
  for (const text of ['拖动旋转 · 滚轮缩放 · 右键平移', '画面：F 聚焦所选，R 复位，Esc 取消选择', '部件：↑↓ 选择，Enter 或双击聚焦，Esc 收起']) helpBody.append(element('div', '', text));
  gestureHint.append(helpTitle, helpBody);
  footer.append(gestureHint, summary);
  root.append(top, toolbar, body, animation, footer); container.append(root);
  const scene = new THREE.Scene(), camera = new THREE.PerspectiveCamera(45, 1, .001, 10000);
  const raycaster = new THREE.Raycaster(), pointer = new THREE.Vector2(), overall = new THREE.Box3();
  let resizeObserver = null, themeObserver = null;
  const handle = { getState, dispose, setActive };
  let inspectorOpen = typeof saved.inspectorOpen === 'boolean' ? saved.inspectorOpen : root.clientWidth >= 720;
  const partsButton = button(top, '部件', () => setInspectorOpen(!inspectorOpen), '显示或收起模型部件');
  partsButton.className = 'kx-model-parts-toggle';
  const closeParts = button(sideHeader, '×', () => { setInspectorOpen(false); partsButton.focus(); }, '收起部件面板'); closeParts.setAttribute('aria-label', '收起部件面板');
  function setInspectorOpen(value) {
    inspectorOpen = !!value; side.hidden = !inspectorOpen; root.dataset.inspector = String(inspectorOpen);
    partsButton.setAttribute('aria-expanded', String(inspectorOpen)); partsButton.setAttribute('aria-pressed', String(inspectorOpen));
    if (renderer) stateChanged();
  }
  setInspectorOpen(inspectorOpen);

  function current() { return !disposed && !signal?.aborted && isCurrent(); }
  function ensureCurrent() { if (!current()) throw abortError(); }
  function button(parent, label, callback, tip) { const b = element('button', '', label); b.type = 'button'; b.setAttribute('aria-label', label); if (tip) b.title = tip; listen(b, 'click', callback); parent.append(b); return b; }
  function stateChanged() {
    if (disposed) return;
    startLoop();
    clearTimeout(stateTimer);
    stateTimer = setTimeout(() => { stateTimer = 0; if (current()) onState?.(getState()); }, 180);
  }
  function getState() {
    if (disposed && snapshot) return structuredClone(snapshot);
    return { version: 1, ...settings, camera: cameraReady ? { position: camera.position.toArray(), target: controls.target.toArray(), up: camera.up.toArray(), zoom: camera.zoom } : undefined,
      selected: selected ? idByObject.get(selected) : null, hidden: [...hidden], isolated,
      clip: clipIndex, time: action ? action.time : 0, playing, inspectorOpen, treeFilter: search.value, sidebarScroll: tree.scrollTop };
  }
  function collect(object) {
    object?.traverse(o => { if (o.geometry) { geometries.add(o.geometry); if (disposed) o.geometry.dispose(); } for (const m of [].concat(o.material || [])) { if (!m) continue; materials.add(m); for (const value of Object.values(m)) if (value?.isTexture) { textures.add(value); if (disposed) { value.dispose(); if (typeof ImageBitmap !== 'undefined' && value.image instanceof ImageBitmap) value.image.close(); } } if (disposed) m.dispose(); } });
  }
  function releaseObject(object) { collect(object); }
  function dispose() {
    if (disposed) return;
    snapshot = getState(); disposed = true; active = false;
    cancelAnimationFrame(raf); clearTimeout(stateTimer); clearTimeout(filterTimer); lifetime.abort(); resizeObserver?.disconnect(); themeObserver?.disconnect(); notice.dispose();
    signal?.removeEventListener('abort', dispose);
    mixer?.stopAllAction(); if (model) mixer?.uncacheRoot(model);
    controls?.dispose(); releaseObject(model); releaseObject(grid); releaseObject(selectionBox);
    for (const m of materials) m.dispose(); for (const g of geometries) g.dispose();
    for (const t of textures) { t.dispose(); if (typeof ImageBitmap !== 'undefined' && t.image instanceof ImageBitmap) t.image.close(); }
    scene.environment = null; envTarget?.dispose(); renderer?.renderLists?.dispose(); renderer?.dispose(); renderer?.forceContextLoss();
    for (const url of objectUrls) URL.revokeObjectURL(url);
    scene.clear(); objectById.clear(); idByObject.clear(); rowById.clear(); materials.clear(); geometries.clear(); textures.clear(); objectUrls.clear();
    model = mixer = action = selected = renderer = controls = grid = selectionBox = envTarget = null;
    root.remove();
  }
  function setActive(value) {
    active = !!value; lastFrame = 0;
    if (disposed) return;
    if (active && !document.hidden) { resize(); startLoop(); } else { cancelAnimationFrame(raf); raf = 0; stateChanged(); }
  }
  function resize() {
    if (disposed || !renderer) return;
    const width = Math.max(1, stage.clientWidth), height = Math.max(1, stage.clientHeight);
    renderer.setSize(width, height, false); camera.aspect = width / height; camera.updateProjectionMatrix();
    startLoop();
  }
  function frame(now) {
    raf = 0;
    if (!current()) { dispose(); return; }
    if (!active || document.hidden || contextLost || renderFailed) return;
    const delta = lastFrame ? Math.min((now - lastFrame) / 1000, .1) : 0; lastFrame = now;
    try {
      const moving = controls.update();
      const animating = mixer && action && playing;
      if (animating) { mixer.update(delta); updateTimeline(); if (now - progressSavedAt > 1200) { progressSavedAt = now; stateChanged(); } }
      if (selectionBox && selected) selectionBox.setFromObject(selected);
      renderer.render(scene, camera);
      // Static views rest; interaction, damping and playback request their own frames.
      // 静止视图停止绘制，操作、阻尼和动画按需唤醒。
      if (moving || animating) startLoop(); else lastFrame = 0;
    } catch {
      renderFailed = true; cancelAnimationFrame(raf); raf = 0;
      notice.show('三维画面绘制失败，请重新打开预览。', true);
    }
  }
  function startLoop() { if (!raf && renderer && controls && active && !document.hidden && !contextLost && !renderFailed && current()) raf = requestAnimationFrame(frame); }
  function updateBackground() {
    const color = BACKGROUNDS[settings.background] || getComputedStyle(document.documentElement).getPropertyValue('--appearance-main').trim() || BACKGROUNDS.light;
    scene.background = new THREE.Color(color); stage.style.background = color;
  }
  function prepareEnvironment() {
    scene.environment = null; envTarget?.dispose(); envTarget = null;
    const pmrem = new THREE.PMREMGenerator(renderer), room = new RoomEnvironment();
    try { envTarget = pmrem.fromScene(room, .04); scene.environment = envTarget.texture; }
    finally { room.dispose(); pmrem.dispose(); }
  }
  function setWireframe(value) { settings.wireframe = value; for (const m of materials) if ('wireframe' in m) { m.wireframe = value; m.needsUpdate = true; } wireButton.setAttribute('aria-pressed', String(value)); stateChanged(); }
  function fit(direction = null) {
    if (!model) return;
    const box = selected && direction === 'selection' ? new THREE.Box3().setFromObject(selected) : overall;
    if (box.isEmpty()) return;
    const center = box.getCenter(new THREE.Vector3()), size = box.getSize(new THREE.Vector3());
    const radius = Math.max(size.length() / 2, .001);
    const vFov = THREE.MathUtils.degToRad(camera.fov), hFov = 2 * Math.atan(Math.tan(vFov / 2) * camera.aspect);
    const distance = radius / Math.sin(Math.min(vFov, hFov) / 2) * 1.15;
    const directions = { front: [0, 0, 1], back: [0, 0, -1], left: [-1, 0, 0], right: [1, 0, 0], top: [0, 1, .0001], home: [1, .7, 1] };
    const dir = directions[direction] ? new THREE.Vector3(...directions[direction]) : camera.position.clone().sub(controls.target);
    if (dir.lengthSq() < 1e-10) dir.set(1, .7, 1);
    camera.up.set(0, 1, 0); camera.zoom = 1; camera.position.copy(center).add(dir.normalize().multiplyScalar(distance));
    controls.target.copy(center); camera.near = Math.max(radius / 10000, .00001); camera.far = Math.max(radius * 10000, 100); camera.updateProjectionMatrix();
    controls.minDistance = radius / 1000; controls.maxDistance = radius * 3000; controls.update(); cameraReady = true; stateChanged();
  }
  function getId(object) { return idByObject.get(object); }
  function dimensions(object) { const b = object ? new THREE.Box3().setFromObject(object) : overall; if (b.isEmpty()) return '无几何尺寸'; const size = b.getSize(new THREE.Vector3()); return `${size.toArray().map(v => Number(v.toPrecision(5))).join(' × ')} 模型单位`; }
  function showSelection() {
    selectionInfo.replaceChildren(element('strong', '', selected ? selected.name || `对象 ${getId(selected)}` : '整个模型'), element('span', '', dimensions(selected)), element('span', '', selected ? selected.type : '点击模型或对象树选择'));
    const activeId = selected && rowById.has(getId(selected)) ? getId(selected) : rowById.keys().next().value;
    for (const [id, row] of rowById) { row.setAttribute('aria-selected', String(selected && id === getId(selected))); row.querySelector('button').tabIndex = id === activeId ? 0 : -1; }
    hideButton.disabled = isolateButton.disabled = focusButton.disabled = !selected;
  }
  function select(object) {
    selected = object || null;
    if (selectionBox) { scene.remove(selectionBox); selectionBox.geometry.dispose(); selectionBox.material.dispose(); selectionBox = null; }
    if (selected) { selectionBox = new THREE.BoxHelper(selected, 0x3987ee); selectionBox.material.depthTest = false; selectionBox.renderOrder = 1000; scene.add(selectionBox); }
    showSelection(); stateChanged();
  }
  function ancestorOf(a, b) { for (let o = b; o; o = o.parent) if (o === a) return true; return false; }
  function updateVisibility() {
    const isolatedObject = isolated ? objectById.get(isolated) : null;
    for (const [id, object] of objectById) {
      object.visible = !hidden.has(id) && (!isolatedObject || ancestorOf(object, isolatedObject) || ancestorOf(isolatedObject, object));
      const row = rowById.get(id); if (row) { row.querySelector('input').checked = !hidden.has(id); row.style.opacity = object.visible ? '1' : '.5'; }
    }
    isolateButton.setAttribute('aria-pressed', String(!!isolated)); if (selectionBox) selectionBox.visible = !!selected && visibleInTree(selected); stateChanged();
  }
  function visibleInTree(o) { for (let item = o; item; item = item.parent) if (!item.visible) return false; return true; }
  function buildTree(object, id = '0') {
    objectById.set(id, object); idByObject.set(object, id); if (!object.visible) hidden.add(id);
    object.children.forEach((child, i) => buildTree(child, `${id}.${i}`));
  }
  function renderTree() {
    const query = search.value.trim().toLocaleLowerCase(); let matches = 0;
    const fragment = document.createDocumentFragment(); rowById.clear();
    for (const [id, object] of objectById) {
      const name = object.name || (object === model ? '模型' : object.type);
      if (query && !`${name} ${object.type} ${id}`.toLocaleLowerCase().includes(query)) continue;
      matches++; if (rowById.size >= 1500) continue;
      const row = element('div', 'kx-model-tree-row'), depth = id.split('.').length - 1;
      row.dataset.objectId = id; row.setAttribute('role', 'treeitem'); row.setAttribute('aria-level', String(depth + 1)); row.style.paddingLeft = `${Math.min(depth, 12) * 10 + 3}px`;
      const visible = element('input'); visible.type = 'checkbox'; visible.tabIndex = -1; visible.checked = !hidden.has(id); visible.setAttribute('aria-label', `显示 ${name}`);
      const label = element('button', '', name); label.type = 'button'; label.title = `${name} · ${object.type} · 双击聚焦`; label.setAttribute('aria-label', name);
      row.append(visible, label); row.style.opacity = visibleInTree(object) ? '1' : '.5'; fragment.append(row); rowById.set(id, row);
    }
    if (!matches) fragment.append(element('div', 'kx-model-tree-empty', '没有匹配的部件，试试其他名称或类型。'));
    tree.replaceChildren(fragment); treeCount.textContent = query ? `匹配 ${matches} / ${objectById.size}` : `${objectById.size} 个部件`;
    if (matches > 1500) treeCount.textContent += ' · 显示前 1500 项';
    showSelection();
  }
  function treeTarget(event) { const row = event.target.closest('.kx-model-tree-row'); return row && tree.contains(row) ? objectById.get(row.dataset.objectId) : null; }
  listen(search, 'input', () => { clearTimeout(filterTimer); filterTimer = setTimeout(() => { filterTimer = 0; renderTree(); stateChanged(); }, 100); });
  listen(tree, 'change', event => { const object = treeTarget(event); if (object && event.target.matches('input[type=checkbox]')) { const id = getId(object); if (event.target.checked) hidden.delete(id); else hidden.add(id); updateVisibility(); } });
  listen(tree, 'click', event => { const object = treeTarget(event); if (object && event.target.closest('button')) select(object); });
  listen(tree, 'dblclick', event => { const object = treeTarget(event); if (object && event.target.closest('button')) { select(object); fit('selection'); } });
  listen(tree, 'keydown', event => {
    const object = treeTarget(event); if (!object || !event.target.matches('button')) return;
    const rows = [...rowById.values()], index = rows.indexOf(event.target.closest('.kx-model-tree-row'));
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault(); const next = event.key === 'Home' ? 0 : event.key === 'End' ? rows.length - 1 : clamp(index + (event.key === 'ArrowDown' ? 1 : -1), 0, rows.length - 1);
      select(objectById.get(rows[next].dataset.objectId)); rows[next].querySelector('button').focus(); rows[next].scrollIntoView({ block: 'nearest' });
    } else if (event.key === 'Enter' || event.key.toLowerCase() === 'f') { event.preventDefault(); select(object); fit('selection'); }
  });

  button(primaryTools, '适配', () => fit(), '适配整个模型'); button(primaryTools, '复位', () => fit('home'), '恢复默认视角');
  function panelToggle(label, panel) {
    const toggle = button(primaryTools, label, () => {
      const open = panel.hidden; viewTools.hidden = displayTools.hidden = true; panel.hidden = !open;
      for (const control of primaryTools.querySelectorAll('[aria-expanded]')) control.setAttribute('aria-expanded', 'false');
      toggle.setAttribute('aria-expanded', String(open));
    });
    toggle.setAttribute('aria-expanded', 'false'); return toggle;
  }
  panelToggle('视角', viewTools); panelToggle('显示', displayTools);
  viewTools.append(element('span', 'kx-model-tools-caption', '标准视图'));
  for (const [name, value] of [['前', 'front'], ['后', 'back'], ['左', 'left'], ['右', 'right'], ['顶', 'top']]) button(viewTools, name, () => fit(value), `${name}视图`);
  const wireButton = button(displayTools, '线框', () => setWireframe(!settings.wireframe)); wireButton.setAttribute('aria-pressed', String(settings.wireframe));
  const gridButton = button(displayTools, '网格', () => { settings.grid = !settings.grid; if (grid) grid.visible = settings.grid; gridButton.setAttribute('aria-pressed', String(settings.grid)); stateChanged(); }); gridButton.setAttribute('aria-pressed', String(settings.grid));
  const bgLabel = element('label', '', '背景'), bg = element('select'); bg.setAttribute('aria-label', '背景颜色');
  for (const [key, label] of [['auto', '跟随主题'], ['light', '浅色'], ['dark', '深色'], ['gray', '灰色'], ['white', '白色']]) { const op = element('option', '', label); op.value = key; bg.append(op); } bg.value = settings.background;
  listen(bg, 'change', () => { settings.background = bg.value; updateBackground(); stateChanged(); }); bgLabel.append(bg); displayTools.append(bgLabel);
  const exposureLabel = element('label', '', '曝光'), exposure = element('input'); exposure.type = 'range'; exposure.min = '.2'; exposure.max = '3'; exposure.step = '.05'; exposure.value = String(settings.exposure); exposure.setAttribute('aria-label', '曝光');
  listen(exposure, 'input', () => { settings.exposure = Number(exposure.value); renderer.toneMappingExposure = settings.exposure; stateChanged(); }); exposureLabel.append(exposure); displayTools.append(exposureLabel);
  const focusButton = button(objectActions, '聚焦', () => fit('selection'));
  const hideButton = button(objectActions, '隐藏', () => { if (selected) hidden.add(getId(selected)); updateVisibility(); });
  const isolateButton = button(objectActions, '隔离', () => { isolated = isolated ? null : getId(selected); updateVisibility(); });
  button(objectActions, '全部显示', () => { hidden.clear(); isolated = null; updateVisibility(); });
  button(objectActions, '引用部件', () => {
    const object = selected || model; if (!object) return;
    options.onSelection?.({ kind: 'model3d', title: options.title || '三维模型', objectId: getId(object),
      text: `对象：${object.name || object.type}\n节点：${getId(object)}\n尺寸：${dimensions(object)}\n类型：${object.type}` });
  });
  const playButton = button(animation, '播放', () => { playing = !playing; updateTimeline(); stateChanged(); });
  const clipSelect = element('select'); clipSelect.setAttribute('aria-label', '动画片段'); animation.append(clipSelect);
  const timeline = element('input'); timeline.type = 'range'; timeline.min = '0'; timeline.max = '1'; timeline.step = '.001'; timeline.value = '0'; timeline.setAttribute('aria-label', '动画进度');
  const timeLabel = element('span', 'kx-model-time', '0.00 / 0.00s'); animation.append(timeline, timeLabel);
  let clips = [];
  function setClip(index, time = 0) {
    if (!mixer || !clips.length) return;
    clipIndex = clamp(Math.trunc(index), 0, clips.length - 1); mixer.stopAllAction();
    action = mixer.clipAction(clips[clipIndex]); action.play(); duration = clips[clipIndex].duration;
    action.time = clamp(num(time), 0, duration); mixer.update(0); clipSelect.value = String(clipIndex); updateTimeline(); stateChanged();
  }
  function updateTimeline() { playButton.textContent = playing ? '暂停' : '播放'; playButton.setAttribute('aria-label', playButton.textContent); playButton.setAttribute('aria-pressed', String(playing)); const time = action?.time || 0; timeline.value = String(duration ? time / duration : 0); timeLabel.textContent = `${time.toFixed(2)} / ${duration.toFixed(2)}s`; }
  listen(clipSelect, 'change', () => setClip(Number(clipSelect.value)));
  listen(timeline, 'input', () => { if (!action) return; action.time = Number(timeline.value) * duration; mixer.update(0); updateTimeline(); stateChanged(); });
  listen(tree, 'scroll', stateChanged, { passive: true });
  listen(side, 'keydown', e => { if (e.key === 'Escape') { e.preventDefault(); if (e.target === search && search.value) { clearTimeout(filterTimer); search.value = ''; renderTree(); stateChanged(); } else { setInspectorOpen(false); partsButton.focus(); } } });
  listen(document, 'visibilitychange', () => setActive(active));
  listen(stage, 'keydown', e => { if (e.target !== stage) return; if (e.key === 'Escape') { if (inspectorOpen && root.clientWidth < 720) { setInspectorOpen(false); partsButton.focus(); } else { select(null); isolated = null; updateVisibility(); } } else if (e.key.toLowerCase() === 'f') fit(selected ? 'selection' : null); else if (e.key.toLowerCase() === 'r') fit('home'); });
  signal?.addEventListener('abort', dispose, { once: true });

  const resourceMap = new Map(), allowedUrls = new Set();
  let mainUrl;
  function authorize(value, topLevel = false) {
    if (value instanceof Blob) { const url = URL.createObjectURL(value); objectUrls.add(url); allowedUrls.add(url); return url; }
    const raw = typeof value === 'string' ? value : value?.contentUrl || value?.url;
    if (typeof raw !== 'string' || !raw) throw new Error('模型附件缺少授权地址。');
    const url = new URL(raw, location.href);
    if (!['https:', 'http:', 'blob:', 'data:'].includes(url.protocol)) throw new Error(`禁止的模型资源协议：${url.protocol}`);
    if (!topLevel && /^https?:$/.test(url.protocol) && url.origin !== mainUrl.origin) throw new Error('模型附件地址必须与授权内容网关同源。');
    if (/^https?:$/.test(url.protocol) && !/^https?:\/\//i.test(raw)) throw new Error('模型资源必须使用明确授权的完整地址。');
    allowedUrls.add(url.href); return url.href;
  }
  function resolveResource(uri) {
    if (allowedUrls.has(uri)) return uri;
    if (/^(data:|blob:)/i.test(uri)) return authorize(uri);
    const key = canonical(uri), mapped = resourceMap.get(uri) || resourceMap.get(key);
    if (mapped) return mapped;
    throw new Error(`缺少已授权的模型附件：${uri}。请提供 resources 映射；预览器不会自行访问磁盘或远程地址。`);
  }
  async function fetchBytes(url, reportProgress = false) {
    const response = await fetch(url, { signal: lifetime.signal, credentials: 'omit', redirect: 'error' });
    if (!response.ok) throw new Error(`模型资源读取失败（HTTP ${response.status}）。`);
    const length = Number(response.headers.get('content-length')); if (length > MAX_BYTES) throw new Error('模型资源超过 256 MB 预览限制。');
    if (!response.body?.getReader) {
      const data = await response.arrayBuffer(); if (data.byteLength > MAX_BYTES) throw new Error('模型资源超过 256 MB 预览限制。'); ensureCurrent(); return data;
    }
    const reader = response.body.getReader(), chunks = []; let received = 0, reportedAt = 0;
    try {
      while (true) {
        const { done, value } = await reader.read(); ensureCurrent(); if (done) break;
        received += value.byteLength;
        if (received > MAX_BYTES) { await reader.cancel(); throw new Error('模型资源超过 256 MB 预览限制。'); }
        chunks.push(value);
        const now = performance.now();
        if (reportProgress && !contextLost && now - reportedAt > 120) {
          reportedAt = now; status.textContent = `正在读取模型 · ${(received / 1048576).toFixed(1)} MB${length > 0 ? ` / ${(length / 1048576).toFixed(1)} MB` : ''}`;
        }
      }
    } finally { reader.releaseLock(); }
    const data = new Uint8Array(received); let offset = 0;
    for (const chunk of chunks) { data.set(chunk, offset); offset += chunk.byteLength; }
    return data.buffer;
  }

  try {
    mainUrl = new URL(authorize(contentUrl, true));
    const entries = Array.isArray(options.resources) ? options.resources.map(r => [r.uri, r]) : Object.entries(options.resources || {});
    for (const [key, value] of entries) { if (key && value && value.available !== false) { const url = authorize(value); resourceMap.set(key, url); resourceMap.set(canonical(key), url); } }
    renderer = new THREE.WebGLRenderer({ antialias: true, alpha: false, powerPreference: 'high-performance' });
    renderer.setPixelRatio(Math.min(devicePixelRatio || 1, 2)); renderer.outputColorSpace = THREE.SRGBColorSpace; renderer.toneMapping = THREE.ACESFilmicToneMapping; renderer.toneMappingExposure = settings.exposure;
    renderer.domElement.setAttribute('aria-label', '可交互 3D 模型，左键旋转、滚轮缩放、右键平移'); stage.prepend(renderer.domElement);
    listen(renderer.domElement, 'webglcontextlost', event => {
      event.preventDefault(); contextLost = true; cancelAnimationFrame(raf); raf = 0; lastFrame = 0; status.hidden = true;
      notice.show('三维画面已暂停，显卡恢复后会保留当前视角继续显示。', true);
    });
    listen(renderer.domElement, 'webglcontextrestored', () => {
      contextLost = renderFailed = false; lastFrame = 0; notice.hide(); status.hidden = !!model;
      if (!model) status.textContent = '正在加载模型…';
      try { prepareEnvironment(); resize(); startLoop(); }
      catch { renderFailed = true; notice.show('三维画面恢复失败，请重新打开预览。', true); }
    });
    controls = new OrbitControls(camera, renderer.domElement); controls.enableDamping = true; controls.dampingFactor = .09;
    controls.addEventListener('change', stateChanged); controls.addEventListener('end', stateChanged);
    updateBackground(); camera.position.set(3, 2, 3); resize();
    themeObserver = new MutationObserver(() => { if (current() && settings.background === 'auto') { updateBackground(); startLoop(); } });
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['style', 'class', 'data-theme'] });
    scene.add(new THREE.HemisphereLight(0xc9ddff, 0x5f5269, 2.1));
    const key = new THREE.DirectionalLight(0xfff3e8, 3.2); key.position.set(4, 7, 5); scene.add(key);
    const fill = new THREE.DirectionalLight(0xaecbff, 1.8); fill.position.set(-5, 2, -3); scene.add(fill);
    prepareEnvironment();
    resizeObserver = new ResizeObserver(resize); resizeObserver.observe(stage);
    let down = null;
    listen(renderer.domElement, 'pointerdown', e => { if (e.button === 0) down = { x: e.clientX, y: e.clientY }; });
    listen(renderer.domElement, 'pointerup', e => {
      if (!down || Math.hypot(e.clientX - down.x, e.clientY - down.y) > 5 || e.button !== 0) { down = null; return; } down = null;
      const r = renderer.domElement.getBoundingClientRect(); pointer.set(((e.clientX - r.left) / r.width) * 2 - 1, -((e.clientY - r.top) / r.height) * 2 + 1);
      if (!model) return;
      raycaster.setFromCamera(pointer, camera); const hit = raycaster.intersectObject(model, true).find(item => visibleInTree(item.object)); select(hit?.object || null);
    });
    const manager = new THREE.LoadingManager(); manager.setURLModifier(resolveResource);
    let attachmentFailure = null, pendingResources = 0; const resourceWaiters = [];
    manager.onError = url => { attachmentFailure = new Error(`模型附件加载失败：${url}`); };
    const originalStart = manager.itemStart, originalEnd = manager.itemEnd;
    manager.itemStart = url => { pendingResources++; originalStart.call(manager, url); };
    manager.itemEnd = url => { originalEnd.call(manager, url); if (--pendingResources === 0) resourceWaiters.splice(0).forEach(resolve => resolve()); };
    async function prepareResource(uri, mime = '') {
      const url = resolveResource(uri);
      if (/^(blob:|data:)/i.test(url)) return url;
      const bytes = await fetchBytes(url), blob = new Blob([bytes], { type: mime || 'application/octet-stream' });
      const local = authorize(blob); resourceMap.set(uri, local); resourceMap.set(canonical(uri), local); return local;
    }
    const data = await fetchBytes(mainUrl.href, true); status.textContent = '正在准备模型材质…';
    let format = (options.renderFormat || options.sourceFormat || '').toLowerCase().replace(/^\./, '');
    if (!['gltf', 'glb', 'obj', 'stl'].includes(format)) format = /\.(gltf|glb|obj|stl)(?:$|[?#])/i.exec(options.title || '')?.[1]?.toLowerCase() || /\.(gltf|glb|obj|stl)(?:$|[?#])/i.exec(mainUrl.pathname)?.[1]?.toLowerCase();
    const magic = data.byteLength >= 4 ? new DataView(data).getUint32(0, true) : 0;
    if (magic === 0x46546c67) format = 'glb';
    if (format === 'gltf' || format === 'glb') {
      let json;
      if (format === 'gltf') json = JSON.parse(new TextDecoder().decode(data));
      else {
        if (data.byteLength < 20) throw new Error('GLB 文件头不完整。');
        const view = new DataView(data), len = view.getUint32(12, true);
        if (view.getUint32(4, true) !== 2) throw new Error('仅支持 glTF 2.0 的 GLB 模型，请重新导出。');
        if (view.getUint32(8, true) !== data.byteLength) throw new Error('GLB 文件长度不匹配，文件可能未下载完整。');
        if (view.getUint32(16, true) !== 0x4e4f534a || len % 4 !== 0 || len > data.byteLength - 20) throw new Error('GLB JSON 数据段无效。');
        json = JSON.parse(new TextDecoder().decode(new Uint8Array(data, 20, len)).replace(/\0+$/, ''));
      }
      const unsupported = (json.extensionsRequired || []).filter(x => ['KHR_draco_mesh_compression', 'EXT_meshopt_compression', 'KHR_texture_basisu'].includes(x));
      if (unsupported.length) throw new Error(`此模型需要尚未随包提供的解码器：${unsupported.join(', ')}。请导出未压缩 GLB/glTF 后预览。`);
      await Promise.all([...(json.buffers || []), ...(json.images || [])].filter(item => item.uri).map(item => prepareResource(item.uri, item.mimeType)));
      const gltf = await new GLTFLoader(manager).parseAsync(format === 'gltf' ? JSON.stringify(json) : data, '');
      if (!current()) { releaseObject(gltf.scene); throw abortError(); } model = gltf.scene; clips = gltf.animations || [];
    } else if (format === 'obj') {
      const text = new TextDecoder().decode(data), objLoader = new OBJLoader(manager);
      const libraries = [...text.matchAll(/^\s*mtllib\s+(.+)$/gm)].map(m => m[1].trim());
      if (libraries.length) {
        const parts = await Promise.all(libraries.map(async uri => ({ uri, text: new TextDecoder().decode(await fetchBytes(resolveResource(uri))) })));
        const allInfo = Object.create(null);
        for (const part of parts) {
          const parsed = new MTLLoader(manager).parse(part.text, '');
          for (const info of Object.values(parsed.materialsInfo)) for (const [key, value] of Object.entries(info)) {
            if (!['map_kd', 'map_ks', 'map_ke', 'norm', 'map_bump', 'bump', 'map_d', 'disp'].includes(key)) continue;
            const textureUri = parsed.getTextureParams(value, {}).url;
            const relative = canonical(new URL(textureUri, new URL(part.uri, 'https://model.invalid/')).pathname.slice(1));
            const resourceKey = resourceMap.has(relative) ? relative : textureUri;
            const local = await prepareResource(resourceKey);
            info[key] = String(value).replace(textureUri, local);
          }
          Object.assign(allInfo, parsed.materialsInfo);
        }
        const creator = new MTLLoader(manager).parse('', ''); creator.setMaterials(allInfo); creator.preload();
        collect({ traverse: callback => callback({ material: Object.values(creator.materials) }) }); objLoader.setMaterials(creator);
      }
      model = objLoader.parse(text);
      model.traverse(o => { if (!o.isMesh || libraries.length) return; const old = o.material; o.material = new THREE.MeshStandardMaterial({ color: 0xb8c8de, roughness: .58, metalness: .12, side: THREE.DoubleSide }); for (const m of [].concat(old || [])) m.dispose(); });
    } else if (format === 'stl') {
      const geometry = new STLLoader(manager).parse(data); if (!geometry.attributes.normal) geometry.computeVertexNormals();
      const material = new THREE.MeshStandardMaterial({ color: geometry.hasColors ? 0xffffff : 0xb8c8de, roughness: .56, metalness: .12, vertexColors: !!geometry.hasColors, opacity: geometry.alpha ?? 1, transparent: (geometry.alpha ?? 1) < 1 });
      model = new THREE.Mesh(geometry, material); model.name = options.title || 'STL 模型';
    } else throw new Error('暂不支持此模型格式。请选择 GLB、glTF、OBJ 或 STL。');
    if (pendingResources) await new Promise(resolve => resourceWaiters.push(resolve));
    ensureCurrent(); if (attachmentFailure) throw attachmentFailure;
    if (!model) throw new Error('模型未包含可显示的场景。');
    scene.add(model); model.updateMatrixWorld(true); overall.setFromObject(model);
    if (overall.isEmpty() || !overall.min.toArray().concat(overall.max.toArray()).every(Number.isFinite)) throw new Error('模型没有有效的几何数据。');
    collect(model); buildTree(model); isolated = typeof saved.isolated === 'string' && objectById.has(saved.isolated) ? saved.isolated : null;
    const size = overall.getSize(new THREE.Vector3()), center = overall.getCenter(new THREE.Vector3()), span = Math.max(size.x, size.y, size.z, .001);
    grid = new THREE.GridHelper(span * 2.5, 20, 0x8b9bb1, 0xb5c0cf); grid.position.set(center.x, overall.min.y - span * .002, center.z); grid.visible = settings.grid; scene.add(grid);
    fit('home');
    if (validVector(saved.camera?.position) && validVector(saved.camera?.target) && validVector(saved.camera?.up)) {
      camera.position.fromArray(saved.camera.position); controls.target.fromArray(saved.camera.target); camera.up.fromArray(saved.camera.up).normalize();
      camera.zoom = clamp(num(saved.camera.zoom, 1), .01, 100); camera.updateProjectionMatrix(); controls.update();
    }
    if (clips.length) { mixer = new THREE.AnimationMixer(model); clips.forEach((clip, i) => { const op = element('option', '', clip.name || `片段 ${i + 1}`); op.value = String(i); clipSelect.append(op); }); animation.hidden = false; setClip(num(saved.clip), num(saved.time)); }
    else playing = false;
    renderTree(); setWireframe(settings.wireframe); select(objectById.get(saved.selected) || null); updateVisibility(); tree.scrollTop = Math.max(0, num(saved.sidebarScroll));
    let triangleCount = 0, meshCount = 0;
    model.traverse(o => { if (o.isMesh) { meshCount++; triangleCount += (o.geometry.index?.count || o.geometry.attributes.position?.count || 0) / 3; } });
    summary.textContent = `${meshCount} 个网格 · ${Math.round(triangleCount).toLocaleString()} 个三角面 · ${format.toUpperCase()}`;
    if (objectById.size > 1500) summary.textContent += ' · 可搜索全部部件';
    status.hidden = true; resize(); if (!contextLost) renderer.render(scene, camera); startLoop(); stateChanged();
    return handle;
  } catch (error) {
    const cancelled = !current() || error?.name === 'AbortError';
    if (cancelled) { dispose(); throw abortError(); }
    const message = /webgl|context/i.test(error?.message || '') ? '无法创建 WebGL 2 渲染上下文。请启用硬件加速或更新显卡驱动后重新打开预览。' : error?.message || '模型加载失败。';
    dispose(); throw new Error(message, { cause: error });
  }
}

window.kynxaModelPreview = Object.freeze({ open, version: '1.0.0', threeVersion: '0.180.0' });
