import { element as el, button, input, field } from './preview-ui.js';
import { createNotice } from './preview-notice.js';
import { createSelectScope } from './workbench-controls.js';
import { createTrainingChart, metricsCsv } from './training-charts.js';
import { createTrainingProgress, duration, latestMetric, statusNames, trainingStatus as status } from './training-progress.js';
import { createTrainingSources } from './training-sources.js';
import { createTrainingDatasetPreview } from './training-datasets.js';
import { createTrainingEnvironment } from './training-environment.js';
import { createTrainingTrial } from './training-trial.js';
import { createTrainingPresets, trainingPreset } from './training-presets.js';
import { createTrainingRecordControls, createTrainingExperimentFilter } from './training-experiments.js';

const titles = { data: '数据集', model: '模型与结构', parameters: '训练参数', monitor: '训练监控', results: '结果' };
const fallbackAlgorithms = { builtin: [['mlp.classifier', '多层感知机分类'], ['mlp.regressor', '多层感知机回归'], ['linear.regression', '线性回归'], ['logistic.regression', '逻辑回归'], ['kmeans', 'K-Means']], sklearn: [['random_forest.classifier', '随机森林分类'], ['random_forest.regressor', '随机森林回归'], ['gradient_boosting.classifier', '梯度提升分类'], ['gradient_boosting.regressor', '梯度提升回归']], torch: [['vision.classifier', '图像分类']], transformers: [['text.lora', '文本 LoRA 微调'], ['text.sft', '文本监督微调'], ['vlm.sft', '视觉语言微调']] };
const algorithmLabels = Object.fromEntries(Object.values(fallbackAlgorithms).flat());
const backendNames = { builtin: '内置轻量训练', sklearn: '机器学习', torch: 'PyTorch 神经网络', transformers: '本地大模型微调' };
backendNames.peft = '大模型 LoRA'; backendNames.trl = '视觉语言微调';
function identity(value) { return value?.id || value?.jobId || value?.datasetId || ''; }
function list(value, key) { return Array.isArray(value) ? value : Array.isArray(value?.[key]) ? value[key] : []; }
function textArea(value, placeholder) { const n = el('textarea'); n.value = value || ''; n.placeholder = placeholder || ''; return n; }
function asList(value) { return Array.isArray(value) ? value : typeof value === 'string' ? value.split(/[,，\s]+/).filter(Boolean) : []; }
function display(value) { if (typeof value === 'number') return Number.isFinite(value) ? Math.abs(value) > 9999 || value !== 0 && Math.abs(value) < .001 ? value.toExponential(3) : Number(value.toPrecision(5)).toString() : '—'; return value == null || value === '' ? '—' : String(value); }
function card(title, description) { const n = el('section', 'workbench-card training-card'); n.append(el('h2', '', title)); if (description) n.append(el('p', '', description)); return n; }
function actions(...nodes) { const n = el('div', 'workbench-row training-actions'); n.append(...nodes); return n; }
function labelled(label, control, help) { if (!control.hasAttribute('aria-label')) control.setAttribute('aria-label', label); const f = field(label, control); if (help) f.append(el('small', 'training-help', help)); return f; }
function idKey() { return crypto.randomUUID(); }
function taskForAlgorithm(value) { return value.startsWith('text.') ? 'text' : value.startsWith('vlm.') ? 'vision_language' : value.startsWith('vision.') ? 'image_classification' : value.includes('regress') ? 'regression' : value === 'kmeans' ? 'clustering' : 'classification'; }

// 页面只管理订阅；切页、切聊天不会取消后台训练。
export async function openTraining(options) {
  const root = el('div', 'training-studio'), banner = el('div', 'training-banner'), nav = el('nav', 'training-tabs'), track = el('div', 'training-tabs-track'), marker = el('span', 'training-tab-marker'), page = el('div', 'training-content');
  const heading = el('div'), badge = el('span', 'training-state', '正在连接'); heading.append(el('h2', '', '模型训练台')); banner.append(heading, badge);
  const feedback = createNotice(root, 'training-notice', { dismissAfterMs: 5000 }), choices = createSelectScope(root);
  let state = { section: 'data', sources: [], datasetName: '', text: '', format: 'auto', targetColumn: '', featureColumns: '', backend: 'builtin', algorithm: 'mlp.classifier', modelPath: '', ...options.state };
  state.parameters = { ...state.parameters }; state.parameterDrafts = { ...state.parameterDrafts }; state.invalidParameters = { ...state.invalidParameters }; state.sources = Array.isArray(state.sources) ? state.sources : []; state.task = taskForAlgorithm(state.algorithm);
  state.dataSplit = { validation: state.parameters.validationSplit ?? .2, test: state.parameters.testSplit ?? .1, ...state.dataSplit };
  let capabilities = {}, datasets = [], jobs = [], job = null, rows = [], comparison = [], chart = null, disposed = false, active = true, timer = null, pollBusy = false, requestSequence = 0, metricCursor = 0, seenMetrics = new Set(), activeJobId = '', idempotencyKey = state.idempotencyKey || null, hasMoreMetrics = false;
  const busy = new Set(); let initialising = true, connected = true, lastRead = 0, liveProgress = null, starting = false, commandPending = false, listRefreshAt = 0, pendingCount = 0, failures = 0;
  const pageViews = new Set();
  let resultFilter = null;
  function own(view) { pageViews.add(view); view.setActive?.(active); return view; }
  function disposePage() { for (const view of pageViews) view.dispose?.(); pageViews.clear(); resultFilter = null; }
  function save() { if (!disposed) options.onState?.({ ...state, parameters: { ...state.parameters }, sources: state.sources.map(s => ({ ...s })) }); }
  function notice(message, error = false) { if (!disposed) feedback.show(message, error); }
  function control(text, fn, primary = false) {
    const b = button(text, async () => {
      if (busy.has(b) || disposed) return; busy.add(b); b.disabled = true; b.setAttribute('aria-busy', 'true'); b.textContent = text + '…';
      try { await fn(); } catch (e) { if (!disposed && e?.name !== 'AbortError') notice(e?.message || String(e), true); }
      finally { busy.delete(b); if (b.isConnected) { b.disabled = b.dataset.locked === 'true'; b.removeAttribute('aria-busy'); b.textContent = text; updateJobControls(); updateStartControls(); } }
    }); if (primary) b.classList.add('primary'); return b;
  }
  async function request(action, payload = {}) { pendingCount++; updateActivity(); try { return await options.request('training.' + action, payload); } finally { pendingCount--; updateActivity(); } }
  function updateActivity() { if (!disposed) root.dataset.busy = String(pendingCount > 0 && (initialising || starting)); }
  async function completeRecord(jobId) {
    let cursor = 0, first, last, all = [], boundary;
    do {
      if (disposed) throw new DOMException('训练页面已关闭', 'AbortError');
      const result = await request('jobs.read', { jobId, afterMetric: cursor, ...(boundary === undefined ? {} : { metricLimit: boundary }) });
      first ||= result; if (boundary === undefined && Number.isFinite(result.metricLength)) boundary = result.metricLength;
      const next = result.nextMetric; all.push(...list(result, 'metrics')); last = result;
      if (!result.hasMoreMetrics || boundary !== undefined && next >= boundary) break;
      if (!Number.isFinite(next) || next <= cursor) throw new Error('指标读取位置没有推进，已保留原记录，请稍后重试导出。');
      cursor = next;
    } while (true);
    return { ...first, job: last.job || first.job, metrics: all, nextMetric: last.nextMetric, hasMoreMetrics: false };
  }
  function invalidateConfiguration() { idempotencyKey = null; delete state.idempotencyKey; }
  // 删除在行内确认，不弹窗；网关仍会拒绝删除正在训练的数据。
  function datasetRemoval(item) {
    const id = identity(item), box = el('div', 'training-dataset-removal');
    function idle() {
      const training = jobs.some(job => job.datasetId === id && ['running', 'queued', 'preparing', 'pausing', 'stopping'].includes(status(job)));
      const open = control('删除数据集', async () => {
        const plan = await request('datasets.delete.preview', { datasetId: id }); if (disposed || !box.isConnected) return;
        const confirm = control('确认删除', async () => {
          await request('datasets.delete.apply', { datasetId: id });
          if (state.datasetId === id) change('datasetId', '');
          await refreshLists(); if (state.section === 'data') navigate('data'); notice('数据集已删除。');
        });
        confirm.classList.add('danger');
        const commands = el('div', 'training-actions'); commands.append(confirm, control('取消', idle));
        box.replaceChildren(el('p', 'training-help', '删除后不能恢复，已有实验的配置和结果会保留。' + (plan.dependentJobs ? `${plan.dependentJobs} 个未完成的图片实验依赖这些图片，删除后无法继续训练。` : '')), commands);
      });
      open.classList.add('training-quiet'); if (training) { open.dataset.locked = 'true'; open.disabled = true; }
      box.replaceChildren(open); if (training) box.append(el('small', 'training-help', '正在训练这份数据，暂停或结束后才能删除。'));
    }
    idle(); return box;
  }
  function change(key, value) { state[key] = value; if (['backend', 'algorithm', 'parameters', 'name', 'datasetId', 'modelPath'].includes(key)) invalidateConfiguration(); save(); updateStartControls(); }
  function parameter(key, value) { if (value === undefined) delete state.parameters[key]; else state.parameters[key] = value; delete state.invalidParameters[key]; invalidateConfiguration(); save(); updateStartControls(); }
  function allowedParameters() { return capabilities.algorithmParameters?.[state.algorithm]; }
  function allows(key) { const allowed = allowedParameters(); return !Array.isArray(allowed) || allowed.includes(key); }
  function namedParameter(key, node) { node.dataset.parameter = key; return node; }
  function filterParameters(container) { container.querySelectorAll('[data-parameter]').forEach(n => { if (!allows(n.dataset.parameter)) n.remove(); }); }
  function select(items, value, action) { return choices.select(items, value, action); }
  function editable(label, value, update, hint) { const n = input(hint || '', value ?? ''); n.oninput = () => update(n.value); return labelled(label, n); }
  function numeric(label, key, fallback, { min, max, step = 'any' } = {}) {
    const n = input('', state.invalidParameters[key]?.value ?? state.parameters[key] ?? fallback); n.type = 'number'; n.step = String(step); if (min !== undefined) n.min = String(min); if (max !== undefined) n.max = String(max);
    const field = namedParameter(key, labelled(label, n)), error = el('small', 'training-field-error'); field.append(error);
    const show = () => { const message = state.invalidParameters[key]?.message; n.setAttribute('aria-invalid', String(!!message)); error.textContent = message || ''; error.hidden = !message; }; show();
    n.oninput = () => {
      const value = Number(n.value), invalid = n.validity.badInput || n.value.trim() && (!Number.isFinite(value) || min !== undefined && value < min || max !== undefined && value > max || step === 1 && !Number.isInteger(value));
      if (invalid) { state.invalidParameters[key] = { value: n.value, message: step === 1 ? '请填写范围内的整数。' : '请填写范围内的有效数值。' }; save(); updateStartControls(); }
      else parameter(key, n.value.trim() ? value : undefined);
      show();
    }; return field;
  }
  function checked(label, key, fallback = false) { const n = el('input'); n.type = 'checkbox'; n.checked = state.parameters[key] ?? fallback; n.onchange = () => parameter(key, n.checked); const f = el('label', 'training-check'); f.append(n, el('span', '', label)); return namedParameter(key, f); }
  function listParameter(label, key, hint, { numbers = false, min = 0, required = false } = {}) {
    const n = input(hint, state.invalidParameters[key]?.value ?? asList(state.parameters[key]).join(', ')), error = el('small', 'training-field-error');
    const host = namedParameter(key, labelled(label, n)); host.append(error);
    const show = () => { error.textContent = state.invalidParameters[key]?.message || ''; error.hidden = !error.textContent; n.setAttribute('aria-invalid', String(!error.hidden)); }; show();
    n.oninput = () => { const values = asList(n.value).map(v => numbers ? Number(v) : v); if (required && !values.length || numbers && values.some(v => !Number.isInteger(v) || v < min)) { state.invalidParameters[key] = { value: n.value, message: min ? '请用逗号分隔正整数，每个数字代表一层。' : '请填写从0开始的整数层编号，留空使用默认范围。' }; save(); updateStartControls(); } else parameter(key, values.length ? values : undefined); show(); }; return host;
  }
  function backendItems() {
    const raw = capabilities.backends;
    if (Array.isArray(raw)) return raw.map(b => typeof b === 'string' ? { id: b, label: backendNames[b] || b, available: true } : { ...b, id: b.id || b.backend, label: b.label || b.name || backendNames[b.id || b.backend] });
    if (raw && typeof raw === 'object') return Object.entries(raw).map(([id, b]) => typeof b === 'boolean' ? { id, available: b, label: backendNames[id] || id } : { ...b, id, label: b.label || backendNames[id] || id });
    return Object.keys(backendNames).map(id => ({ id, label: backendNames[id], available: false, reason: '尚未报告可用训练环境' }));
  }
  function backend() { return backendItems().find(b => b.id === state.backend); }
  function algorithmItems() { const b = backend(), raw = b?.algorithms || capabilities.algorithms?.[state.backend]; return Array.isArray(raw) && raw.length ? raw.map(v => typeof v === 'string' ? [v, algorithmLabels[v] || v] : [v.id || v.algorithm, v.label || v.name || v.id]) : fallbackAlgorithms[state.backend] || []; }
  function defaultParameters(algorithm) { return { ...structuredClone(capabilities.algorithmDefaults?.[algorithm] || {}), loggingSteps: 1 }; }
  function chooseAlgorithm(value) {
    if (value === state.algorithm) return;
    state.parameterDrafts[state.algorithm] = { parameters: structuredClone(state.parameters), invalidParameters: structuredClone(state.invalidParameters) };
    const draft = state.parameterDrafts[value]; state.parameters = draft ? structuredClone(draft.parameters) : defaultParameters(value); state.invalidParameters = structuredClone(draft?.invalidParameters || {});
    change('algorithm', value); change('task', taskForAlgorithm(value));
  }
  function positionMarker() { const selected = track.querySelector('[aria-selected=true]'); if (selected) { marker.style.width = selected.offsetWidth + 'px'; marker.style.height = selected.offsetHeight + 'px'; marker.style.transform = `translate(${selected.offsetLeft}px,${selected.offsetTop}px)`; } }
  function navigate(section) {
    if (disposed) return; choices.close(); chart?.dispose(); disposePage(); chart = null; liveProgress = null; state.section = titles[section] ? section : 'data'; save();
    track.querySelectorAll('button').forEach(b => { const selected = b.dataset.section === state.section; b.setAttribute('aria-selected', String(selected)); b.tabIndex = selected ? 0 : -1; if (selected) page.setAttribute('aria-labelledby', b.id); }); page.replaceChildren();
    if (initialising) page.append(card('正在加载训练台…'));
    else ({ data: renderData, model: renderModel, parameters: renderParameters, monitor: renderMonitor, results: renderResults }[state.section])(); positionMarker();
    const selected = track.querySelector('[aria-selected=true]'); if (selected) nav.scrollTo({ left: Math.max(0, selected.offsetLeft - (nav.clientWidth - selected.offsetWidth) / 2), behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : 'smooth' });
  }
  nav.setAttribute('role', 'tablist'); nav.setAttribute('aria-label', '训练台页面'); page.setAttribute('role', 'tabpanel'); page.id = 'training-content'; track.append(marker); nav.append(track);
  for (const [id, title] of Object.entries(titles)) { const b = button(title, () => navigate(id)); b.dataset.section = id; b.id = 'training-tab-' + id; b.setAttribute('role', 'tab'); b.setAttribute('aria-controls', page.id); track.append(b); }
  nav.addEventListener('keydown', event => { if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return; const buttons = [...track.querySelectorAll('button')], at = buttons.indexOf(document.activeElement); if (at < 0) return; event.preventDefault(); const index = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (at + (event.key === 'ArrowRight' ? 1 : -1) + buttons.length) % buttons.length; buttons[index].focus(); navigate(buttons[index].dataset.section); });
  const reconnect = control('重新连接', async () => { if (!capabilities.backends) await initialise(); else { await refreshLists(); if (state.jobId) await readJob(state.jobId); else { connected = true; reconnect.hidden = true; badge.textContent = '可以配置训练'; } } }); reconnect.hidden = true;
  const connection = el('div', 'training-connection'); connection.append(badge, reconnect); banner.append(connection);
  const resize = new ResizeObserver(positionMarker); resize.observe(track); root.append(banner, nav, feedback.node, page); options.container.append(root);

  function renderData() {
    const grid = el('div', 'training-grid'), source = card('训练资料'), sourceRows = el('div', 'training-source-list');
    source.append(editable('数据集名称', state.datasetName, v => change('datasetName', v), '例如：产品问答或图像分类'));
    const sourceView = own(createTrainingSources(sourceRows, { getSources: () => state.sources, onChange: value => change('sources', value), control, request }));
    async function pick(folder = false) { const result = await request(folder ? 'folder.pick' : 'files.pick', { role: 'data' }); const picked = list(result, 'sources'); if (result?.path && !picked.some(s => s.path === result.path)) picked.push({ path: result.path, name: result.name }); const known = new Set(state.sources.map(s => s.path)); change('sources', [...state.sources, ...picked.filter(s => s?.path && !known.has(s.path) && known.add(s.path))]); sourceView.render(); }
    source.append(actions(control('选择文件', () => pick()), control('导入文件夹', () => pick(true))), sourceRows);
    const plain = textArea(state.text, '粘贴原文、JSONL问答或带表头的CSV数据…'); plain.setAttribute('aria-label', '训练文本或结构化样本'); plain.oninput = () => change('text', plain.value);
    source.append(labelled('文本与结构化样本', plain)); source.append(labelled('资料类型', select([['auto', '自动识别'], ['text', '文本语料'], ['conversation', '问答与多轮对话'], ['table', '表格数据'], ['image', '图像及标签']], state.format, v => change('format', v))));
    const detail = el('details', 'training-details'); detail.append(el('summary', '', '表格字段与数据划分'));
    const mapping = el('div', 'training-fields'); mapping.append(editable('目标／标签列', state.targetColumn, v => change('targetColumn', v), '例如：label'), editable('特征列', state.featureColumns, v => change('featureColumns', v), '逗号分隔，留空自动选择'));
    for (const [key, label] of [['validation', '验证集比例'], ['test', '测试集比例']]) { const n = input('', state.dataSplit[key]); n.type = 'number'; n.min = '0'; n.max = '.9'; n.step = 'any'; n.oninput = () => { state.dataSplit[key] = n.value; n.setAttribute('aria-invalid', String(!n.value.trim() || !n.checkValidity())); save(); }; mapping.append(labelled(label, n)); }
    detail.append(mapping); source.append(detail);
    source.append(actions(control('整理并保存数据集', async () => {
      if (!state.sources.length && !state.text.trim()) throw new Error('先选择训练资料或填写样本。');
      const split = { validation: Number(state.dataSplit.validation), test: Number(state.dataSplit.test) };
      if (Object.values(state.dataSplit).some(v => String(v).trim() === '') || Object.values(split).some(v => !Number.isFinite(v) || v < 0 || v > .9) || split.validation + split.test >= 1) throw new Error('验证集与测试集比例应在0到0.9之间，合计小于1。');
      const result = await request('datasets.create', { name: state.datasetName.trim() || '训练数据集', sources: state.sources, text: state.text, format: state.format, targetColumn: state.targetColumn, featureColumns: asList(state.featureColumns), split, validationSplit: split.validation, testSplit: split.test });
      const dataset = result.dataset || result; change('datasetId', identity(dataset)); await refreshLists(); if (state.section === 'data') navigate('data'); notice('数据集已保存。');
    }, true))); sourceView.render();
    const library = card('已保存的数据集');
    if (!datasets.length) library.append(el('p', 'training-empty', '还没有数据集。'));
    for (const item of datasets) { const n = el('div', 'training-list-item'); n.classList.toggle('selected', identity(item) === state.datasetId); n.append(el('strong', '', item.name || identity(item)), el('p', '', `${item.sampleCount ?? item.count ?? item.samples?.length ?? '—'} 条样本 · ${item.format || item.kind || '自动识别'}`)); if (item.warnings?.length) n.append(el('p', '', item.warnings.join('；'))); n.append(control(identity(item) === state.datasetId ? '当前数据集' : '选择数据集', () => { change('datasetId', identity(item)); navigate('data'); }));
      const details = el('details', 'training-details'), previewBody = el('div'); details.append(el('summary', '', '预览数据'), previewBody); const preview = own(createTrainingDatasetPreview(previewBody, { request, control })); let loaded = false;
      details.addEventListener('toggle', () => { if (details.open && !loaded) { loaded = true; void preview.show(item); } }); n.append(details, datasetRemoval(item)); library.append(n); }
    library.append(control('刷新数据集', async () => { await refreshLists(); if (state.section === 'data') navigate('data'); })); grid.append(source, library); page.append(grid);
    const next = control('下一步：选择模型', () => navigate('model'), true); next.disabled = !state.datasetId; page.append(actions(next));
  }

  function renderModel() {
    const settings = card('训练模型'), fields = el('div', 'training-fields');
    fields.append(labelled('训练环境', select(backendItems().map(b => [b.id, `${b.label || backendNames[b.id] || b.id}${b.available === false ? ' · 未就绪' : ''}`]), state.backend, value => { change('backend', value); chooseAlgorithm(algorithmItems()[0]?.[0] || ''); navigate('model'); })));
    const algorithms = algorithmItems(); if (!algorithms.some(([id]) => id === state.algorithm)) chooseAlgorithm(algorithms[0]?.[0] || '');
    fields.append(labelled('模型／算法', select(algorithms, state.algorithm, value => { chooseAlgorithm(value); navigate('model'); })));
    settings.append(fields); const selected = backend(); if (selected?.available === false) settings.append(el('p', 'training-capability-note', selected.reason || '此训练环境尚未准备好，仍可以配置实验。'));
    const models = list(capabilities.models, 'models'); if (models.length && ['transformers', 'peft', 'trl'].includes(state.backend)) settings.append(labelled('本地模型目录', select([['', '选择已下载的模型'], ...models.map(m => [m.path || m.modelPath || m.id, `${m.name || m.id}${m.trainable === false ? ' · 需要训练底座' : ''}`])], state.modelPath, value => { change('modelPath', value); navigate('model'); })));
    if (['transformers', 'peft', 'trl'].includes(state.backend)) { settings.append(editable('模型训练底座目录', state.modelPath, v => change('modelPath', v), '选择含模型配置和可训练权重的本机目录'), actions(control('选择模型目录', async () => { const result = await request('folder.pick', { role: 'model' }); if (result.modelPath || result.path || result.sources?.[0]?.path) { change('modelPath', result.modelPath || result.path || result.sources[0].path); navigate('model'); } })));
      const selectedModel = models.find(m => [m.path, m.modelPath, m.id].includes(state.modelPath)); if (selectedModel?.reason || selectedModel?.trainingReason) settings.append(el('p', 'training-capability-note', selectedModel.reason || selectedModel.trainingReason));
      const structure = selectedModel?.structure || selectedModel?.architecture; if (structure) { const details = el('details', 'training-details'); details.append(el('summary', '', '模型结构与可训练范围'), el('pre', 'training-json', typeof structure === 'string' ? structure : JSON.stringify(structure, null, 2))); settings.append(details); }
    }
    const layers = card('结构与训练范围');
    const layerFields = el('div', 'training-fields');
    if (state.algorithm.startsWith('mlp.') || state.algorithm === 'vision.classifier') layerFields.append(listParameter(state.algorithm === 'vision.classifier' ? '卷积层通道数' : '隐藏层宽度', 'hiddenLayers', '例如：64,32，每个数字代表一层', { numbers: true, min: 1, required: true }), namedParameter('activation', labelled('激活函数', select([['relu', 'ReLU'], ['tanh', 'Tanh']], state.parameters.activation || 'relu', v => parameter('activation', v)))));
    if (allows('layersToTransform')) layerFields.append(listParameter('训练层编号', 'layersToTransform', '逗号分隔，留空使用后端默认范围', { numbers: true }));
    if (['transformers', 'peft', 'trl'].includes(state.backend)) { layerFields.append(listParameter('目标模块', 'targetModules', '例如：q_proj,v_proj'), numeric('LoRA Rank', 'loraRank', state.algorithm === 'vlm.sft' ? '' : 8, { min: 1, step: 1 }), numeric('LoRA Alpha', 'loraAlpha', state.algorithm === 'vlm.sft' ? '' : 16, { min: 1 }), numeric('LoRA Dropout', 'loraDropout', state.algorithm === 'vlm.sft' ? '' : .05, { min: 0, max: .95, step: .05 })); }
    filterParameters(layerFields);
    layers.hidden = !layerFields.childElementCount;
    layers.append(layerFields); const environment = el('details', 'workbench-card training-card training-environment'); environment.open = selected?.available === false; environment.append(el('summary', '', '运行环境'));
    own(createTrainingEnvironment(environment, { request, control, notice, backend: () => state.backend, onReady: async () => { capabilities = await request('capabilities'); if (!disposed && state.section === 'model') navigate('model'); } }));
    const python = capabilities.executable || capabilities.environment?.python || capabilities.pythonPath || capabilities.environment?.pythonPath || capabilities.environment?.path; environment.append(el('p', 'training-artifact', python || '尚未配置可用Python环境'));
    environment.append(actions(control('选择 Python 环境', async () => { await request('files.pick', { role: 'python' }); capabilities = await request('capabilities'); if (state.section === 'model') navigate('model'); }), control('刷新环境', async () => { capabilities = await request('capabilities'); if (state.section === 'model') navigate('model'); })));
    const modules = capabilities.modules || capabilities.environment?.modules; if (modules && typeof modules === 'object') { const dependencies = el('div', 'training-dependencies'); for (const [name, available] of Object.entries(modules)) { const n = el('span', available ? 'ready' : '', name + (available ? ' · 已就绪' : ' · 未安装')); dependencies.append(n); } environment.append(dependencies); }
    page.append(settings, layers, environment, actions(control('设置训练参数', () => navigate('parameters'), true)));
  }

  function renderParameters() {
    const section = card('训练参数'), fields = el('div', 'training-fields');
    createTrainingPresets(section, { control, apply: kind => { state.parameters = Object.fromEntries(Object.entries(trainingPreset(state.algorithm, defaultParameters(state.algorithm), kind)).filter(([key]) => allows(key))); state.invalidParameters = {}; invalidateConfiguration(); save(); navigate('parameters'); }, estimate: () => request('resources.preview', configuration()) });
    section.append(el('p', 'training-model-label', algorithmLabels[state.algorithm] || state.algorithm));
    fields.append(editable('实验名称', state.name, v => change('name', v), '留空按模型命名'), labelled('训练数据集', select(datasets.map(d => [identity(d), d.name || identity(d)]), state.datasetId || '', v => change('datasetId', v))));
    const neural = state.algorithm !== 'kmeans' && (['builtin', 'torch', 'transformers', 'peft', 'trl'].includes(state.backend));
    if (neural) fields.append(numeric('训练轮数', 'epochs', '', { min: 1, step: 1 }), numeric('批大小', 'batchSize', '', { min: 1, step: 1 }), numeric('学习率', 'learningRate', '', { min: 1e-10 }));
    if (/forest|boost/.test(state.algorithm)) fields.append(numeric('树的数量', 'estimators', 100, { min: 1, step: 1 }), numeric('最大深度（留空不指定）', 'maxDepth', '', { min: 1, step: 1 }));
    if (/boost/.test(state.algorithm)) fields.append(numeric('学习率', 'learningRate', .1, { min: 1e-10 }));
    if (/kmeans/.test(state.algorithm)) fields.append(numeric('聚类数量', 'clusters', 3, { min: 2, step: 1 }), numeric('最大迭代次数', 'epochs', 100, { min: 1, step: 1 })); section.append(fields);
    const advanced = el('details', 'training-details'); advanced.append(el('summary', '', '高级设置')); const more = el('div', 'training-fields');
    more.append(numeric('随机种子', 'seed', 42, { min: 0, step: 1 }));
    if (neural) more.append(numeric('最大训练步数（留空按轮数）', 'maxSteps', '', { min: 1, step: 1 }), namedParameter('optimizer', labelled('优化器', select([['adam', 'Adam'], ['sgd', 'SGD']], state.parameters.optimizer || 'adam', v => parameter('optimizer', v)))), numeric('权重衰减', 'weightDecay', 0, { min: 0 }), numeric('梯度裁剪', 'gradientClipNorm', 1, { min: 0 }), numeric('检查点间隔（步）', 'checkpointEvery', 1, { min: 1, step: 1 }), numeric('早停耐心（0为关闭）', 'earlyStoppingPatience', 0, { min: 0, step: 1 }));
    more.append(namedParameter('device', labelled('计算设备', select([['auto', '自动'], ['cpu', 'CPU'], ['cuda', 'NVIDIA CUDA']], state.parameters.device || 'auto', v => parameter('device', v)))), numeric('CPU线程', 'cpuThreads', 2, { min: 1, step: 1 }), numeric('内存预算 MiB', 'memoryMiB', '', { min: 1, step: 1 }), numeric('显存预算 MiB', 'gpuMemoryMiB', '', { min: 0, step: 1 }));
    if (['transformers', 'peft', 'trl', 'torch'].includes(state.backend)) more.append(namedParameter('precision', labelled('数值精度', select([['auto', '自动'], ['fp32', 'FP32'], ['fp16', 'FP16'], ['bf16', 'BF16']], state.parameters.precision || 'auto', v => parameter('precision', v)))));
    if (['transformers', 'peft', 'trl'].includes(state.backend)) more.append(numeric('文本最大长度', 'maxLength', 512, { min: 16, step: 1 }), numeric('梯度累积步数', 'gradientAccumulationSteps', 1, { min: 1, step: 1 }), numeric('预热步数', 'warmupSteps', 0, { min: 0, step: 1 }), checked('梯度检查点', 'gradientCheckpointing'));
    more.append(numeric('指标记录间隔（步）', 'loggingSteps', 1, { min: 1, step: 1 }));
    if (state.algorithm === 'vision.classifier') more.append(numeric('图像尺寸', 'imageSize', 64, { min: 8, step: 1 }));
    more.append(namedParameter('learningRateSchedule', labelled('学习率调度', select([['constant', '固定'], ['linear', '线性'], ['cosine', '余弦']], state.parameters.learningRateSchedule || 'constant', v => parameter('learningRateSchedule', v))))); filterParameters(fields); filterParameters(more);
    advanced.append(more); section.append(advanced);
    const config = el('details', 'training-details'); config.append(el('summary', '', '本算法完整参数 JSON')); const editor = textArea(JSON.stringify(configuration().parameters, null, 2)); editor.classList.add('training-config-editor'); editor.setAttribute('aria-label', '完整训练参数JSON'); config.append(editor, actions(control('应用参数', () => { const next = JSON.parse(editor.value); if (!next || typeof next !== 'object' || Array.isArray(next)) throw new Error('参数需要是JSON对象。'); const unknown = Object.keys(next).filter(key => !allows(key)); if (unknown.length) throw new Error('当前算法不适用这些参数：' + unknown.join('、')); state.invalidParameters = {}; change('parameters', next); navigate('parameters'); notice('参数已更新。'); }), control('导出配置', () => exportText('训练配置.json', JSON.stringify(configuration(), null, 2), 'application/json')))); section.append(config);
    const readiness = el('div', 'training-readiness'); readiness.dataset.live = 'readiness'; section.append(readiness);
    const start = control('开始训练', startTraining, true); start.setAttribute('aria-label', '创建并开始训练'); start.dataset.trainingCreate = 'true'; section.append(actions(control('返回模型设置', () => navigate('model')), start)); page.append(section); updateStartControls();
  }
  function configuration() { const parameters = Object.fromEntries(Object.entries(state.parameters).filter(([key, value]) => allows(key) && !['validationSplit', 'testSplit'].includes(key) && value !== '' && value !== null)); return { name: state.name?.trim() || `${algorithmItems().find(([id]) => id === state.algorithm)?.[1] || state.algorithm} 训练`, datasetId: state.datasetId, backend: state.backend, algorithm: state.algorithm, task: taskForAlgorithm(state.algorithm), modelPath: ['builtin', 'sklearn', 'torch'].includes(state.backend) ? undefined : state.modelPath || undefined, parameters }; }
  function configurationIssues() {
    const issues = [];
    if (!datasets.some(d => identity(d) === state.datasetId)) issues.push('先保存并选择训练数据集');
    if (!backend() || backend().available === false) issues.push('在模型与结构中准备当前训练环境');
    if (!algorithmItems().some(([id]) => id === state.algorithm)) issues.push('选择当前环境支持的算法');
    if (['transformers', 'peft', 'trl'].includes(state.backend) && !String(state.modelPath || '').trim()) issues.push('选择可训练的模型底座');
    if (list(capabilities.models, 'models').some(m => [m.path, m.modelPath, m.id].includes(state.modelPath) && m.trainable === false) && ['transformers', 'peft', 'trl'].includes(state.backend)) issues.push('当前推理权重需换成兼容的训练底座');
    if (Object.keys(state.invalidParameters).some(allows)) issues.push('修正标红的参数后再开始');
    return issues;
  }
  function updateStartControls() {
    if (disposed) return; const issues = configurationIssues();
    page.querySelectorAll('[data-training-create]').forEach(b => { b.disabled = initialising || starting || issues.length > 0 || busy.has(b); b.setAttribute('aria-busy', String(starting)); });
    const host = page.querySelector('[data-live=readiness]'); if (host) { host.dataset.ready = String(!issues.length); host.hidden = !starting && !issues.length; host.textContent = starting ? '正在准备训练…' : issues.join('；') + '。'; }
  }
  function exportText(filename, content, mimeType, encoding = 'utf8') { options.onExport?.({ filename, content, mimeType, encoding }); }
  async function startTraining() {
    if (starting) return; const issues = configurationIssues(); if (issues.length) throw new Error(issues.join('；') + '。');
    starting = true; updateStartControls();
    try {
      idempotencyKey ||= idKey(); state.idempotencyKey = idempotencyKey; save(); const response = await request('jobs.create', { ...configuration(), idempotencyKey }); const created = response.job || response;
      const jobId = identity(created); if (!jobId) throw new Error('训练作业没有返回有效身份，未派发启动。'); state.jobId = jobId; save();
      await request('jobs.start', { jobId }); await readJob(jobId); if (!disposed) navigate('monitor'); notice('训练已提交。');
    } finally { starting = false; updateStartControls(); updateActivity(); }
  }

  function renderMonitor() {
    const controls = card(job?.name || '当前实验'), choicesRow = el('div', 'training-fields'); choicesRow.append(labelled('训练记录', select(jobs.map(j => [identity(j), `${j.name || identity(j)} · ${statusNames[status(j)] || status(j)}`]), state.jobId || '', async value => { change('jobId', value); try { await readJob(value); if (!disposed && state.section === 'monitor') navigate('monitor'); } catch (e) { notice(e.message, true); } })), labelled('对照实验', select([['', '不对照'], ...jobs.filter(j => identity(j) !== state.jobId).map(j => [identity(j), j.name || identity(j)])], state.compareJobId || '', readComparison)));
    const selection = el('details', 'training-details training-experiment-selector'); selection.open = !job; selection.append(el('summary', '', '切换实验与对照'), choicesRow); controls.append(selection);
    const commands = actions(control('刷新', async () => { await refreshLists(); if (state.jobId) await readJob(state.jobId); if (state.section === 'monitor') navigate('monitor'); }));
    for (const [action, title] of [['start', '开始训练'], ['pause', '暂停'], ['resume', '继续训练'], ['stop', '停止训练']]) { const b = control(title, () => jobAction(action), action === 'start' || action === 'resume'); b.dataset.trainingAction = action; commands.append(b); }
    controls.classList.add('training-session'); const heading = controls.querySelector('h2'), header = el('div', 'training-session-header'); heading.title = job?.name || '当前实验'; header.append(heading, commands); controls.prepend(header);
    if (!job) controls.append(el('p', 'training-empty', '准备数据和参数后开始训练。')); else {
      const summary = el('div', 'training-metrics'); summary.dataset.live = 'summary'; controls.append(summary); renderSummary(summary);
      if (job.resumeSupported === false || job.capabilities?.resume === false) controls.append(el('p', 'training-capability-note', '此算法不支持中途续训。'));
    }
    liveProgress = createTrainingProgress(controls); liveProgress.update(job, { connected, lastRead });
    const resultButton = control('查看训练结果', () => navigate('results'), true); resultButton.dataset.trainingResult = 'true'; commands.append(resultButton);
    page.append(controls); const graphs = card('训练曲线'); graphs.classList.add('training-chart-card'); page.append(graphs); chart = createTrainingChart(graphs, { rows, comparison, liveState: status(job), viewState: state.chartViews?.[state.jobId], onViewState: value => { state.chartViews = { ...state.chartViews, [state.jobId]: value }; save(); }, onExport: async (name, content, mime, encoding) => { try { if (mime === 'text/csv' && state.jobId) { const record = await completeRecord(state.jobId); exportText(name, metricsCsv(record.metrics), mime); } else exportText(name, content, mime, encoding); } catch (e) { notice(e.message, true); } } });
    const logGroup = el('details', 'workbench-card training-card training-technical'), log = el('div'); logGroup.append(el('summary', '', '运行信息与检查点'), log); log.dataset.live = 'details'; page.append(logGroup); renderRunDetails(log); updateJobControls();
  }
  async function readComparison(value) { change('compareJobId', value); comparison = []; chart?.update(rows, comparison); if (!value) return; try { const result = await completeRecord(value); if (disposed || state.compareJobId !== value) return; comparison = list(result, 'metrics'); chart?.update(rows, comparison); } catch (error) { notice(error.message, true); } }
  function updateJobControls() {
    const code = status(job), canResume = job?.canResume ?? job?.state?.canResume;
    const allowed = { start: ['created', 'ready'].includes(code), pause: ['running', 'queued', 'preparing'].includes(code), resume: ['paused', 'interrupted', 'stopped', 'failed'].includes(code) && canResume !== false, stop: ['running', 'queued', 'preparing', 'pausing', 'paused'].includes(code) };
    page.querySelectorAll('[data-training-action]').forEach(b => { b.disabled = !job || !connected || commandPending || !allowed[b.dataset.trainingAction] || busy.has(b); b.hidden = !job || !allowed[b.dataset.trainingAction] && !busy.has(b); });
    page.querySelectorAll('[data-training-result]').forEach(b => b.hidden = code !== 'completed');
  }
  function renderSummary(host) {
    const current = job?.state && typeof job.state === 'object' ? job.state : job || {}, latest = rows.at(-1) || {}, progress = current.progress || job?.progress || {};
    const loss = latestMetric(rows, ['loss', 'trainLoss', 'train_loss']), validationLoss = latestMetric(rows, ['evalLoss', 'eval_loss', 'validationLoss']);
    const facts = [];
    if (loss !== undefined || job?.algorithm !== 'kmeans') facts.push(['训练损失', loss]);
    else facts.push(['聚类误差', latestMetric(rows, ['inertia'])]);
    if (validationLoss !== undefined) facts.push(['验证损失', validationLoss]);
    facts.push(['用时', duration(current.elapsedSeconds ?? latest.elapsedSeconds)], ['训练进度', `${display(current.step ?? progress.step ?? latest.step)} 步 · ${display(current.epoch ?? progress.epoch ?? latest.epoch)} 轮`]);
    const remaining = current.remainingSeconds ?? progress.remainingSeconds; if (Number.isFinite(remaining) && status(job) === 'running') facts.push(['预计剩余', duration(remaining)]);
    const speed = latestMetric(rows, ['samplesPerSecond', 'samples_per_second']); if (speed !== undefined) facts.push(['样本/秒', speed]);
    const signature = JSON.stringify(facts); if (host.dataset.signature === signature) return; host.dataset.signature = signature; host.replaceChildren();
    for (const [label, value] of facts) { const n = el('div', 'training-metric'); n.append(el('strong', '', display(value)), el('span', '', label)); host.append(n); }
  }
  function renderRunDetails(host) { if (!job) { host.replaceChildren(el('h2', '', '运行信息'), el('p', 'training-empty', '训练开始后显示状态、检查点与产物。')); return; } const stateValue = typeof job.state === 'object' ? job.state : {}; const values = [['作业', identity(job)], ['训练环境', backendNames[job.backend] || job.backend], ['算法', algorithmLabels[job.algorithm] || job.algorithm], ['数据集', datasets.find(d => identity(d) === job.datasetId)?.name || job.datasetId], ['检查点', stateValue.checkpoint || job.checkpointPath || job.checkpoint], ['可训练参数', stateValue.trainableParameters ?? job.trainableParameters], ['资源状态', stateValue.resourceState || job.resourceState]]; const signature = JSON.stringify(values); if (host.dataset.signature === signature) return; host.dataset.signature = signature; host.replaceChildren(el('h2', '', '运行信息')); const dl = el('dl', 'training-facts'); for (const [label, value] of values) if (value != null) dl.append(el('dt', '', label), el('dd', '', typeof value === 'object' ? JSON.stringify(value) : String(value))); host.append(dl); }
  async function jobAction(action) {
    if (!state.jobId || commandPending) return; commandPending = true; updateJobControls(); const selectedId = state.jobId;
    try { await request('jobs.' + action, { jobId: selectedId }); if (state.jobId === selectedId) await readJob(selectedId); notice(({ start: '训练已提交。', pause: '已请求在可保存的位置暂停。', resume: '已请求从原实验继续。', stop: '已请求停止训练并保留已有结果。' })[action]); }
    finally { commandPending = false; updateJobControls(); }
  }

  function predictionCard(item) {
    const holder = el('div');
    own(createTrainingTrial(holder, { item, request, notice, state: state.trials?.[identity(item)] || { json: state.predictionInputs?.[identity(item)] || '' }, onState: value => { state.trials = { ...state.trials, [identity(item)]: value }; save(); } }));
    return holder;
  }
  function canTrial(item) { return status(item) === 'completed' && (capabilities.predictionAlgorithms?.includes(item.algorithm) || ['text.sft', 'text.lora'].includes(item.algorithm)); }

  function renderResults() {
    const results = card('训练结果'); const filter = resultFilter = createTrainingExperimentFilter(results, { select, state, onState: save });
    if (!jobs.length) results.append(el('p', 'training-empty', '尚无训练记录。'));
    for (const item of jobs) { const node = el('div', 'training-list-item'); node.dataset.jobId = identity(item); node.dataset.status = status(item); node.dataset.search = [item.name, item.notes, item.algorithm].join(' ').toLocaleLowerCase(); node.append(el('strong', '', item.name || identity(item)), el('p', 'training-result-state', `${backendNames[item.backend] || item.backend || ''} · ${algorithmLabels[item.algorithm] || item.algorithm || ''} · ${statusNames[status(item)] || status(item)}`));
      node.append(actions(control('查看实验', async () => { change('jobId', identity(item)); await readJob(identity(item)); navigate('monitor'); }), control('复制参数', () => { const config = item.config || item; chooseAlgorithm(config.algorithm || item.algorithm); for (const key of ['datasetId', 'backend', 'algorithm', 'task', 'modelPath']) if (config[key] !== undefined) state[key] = config[key]; if (config.parameters) state.parameters = structuredClone(config.parameters); state.invalidParameters = {}; state.name = (item.name || '训练') + ' · 新实验'; invalidateConfiguration(); save(); navigate('parameters'); }), control('导出记录', async () => { const result = await completeRecord(identity(item)); exportText('训练记录-' + identity(item) + '.json', JSON.stringify(result, null, 2), 'application/json'); })));
      node.append(control('导出训练报告', async () => {
        const result = await completeRecord(identity(item)), last = result.metrics.at(-1) || {};
        const report = `# ${result.job.name || '训练报告'}\n\n- 作业：${identity(result.job)}\n- 算法：${result.job.algorithm}\n- 状态：${statusNames[status(result.job)] || status(result.job)}\n- 数据集：${result.job.datasetId}\n- 指标记录：${result.metrics.length}条\n\n## 最后一次实际指标\n\n| 指标 | 值 |\n| --- | --- |\n${Object.entries(last).filter(([, v]) => ['string', 'number', 'boolean'].includes(typeof v)).map(([key, value]) => `| ${key} | ${String(value).replaceAll('|', '\\|')} |`).join('\n')}\n\n## 冻结训练参数\n\n\`\`\`json\n${JSON.stringify(result.job.parameters || {}, null, 2)}\n\`\`\`\n\n## 产物\n\n${(result.artifacts || []).map(a => '- ' + (a.title || a.kind || '文件') + '：' + a.path).join('\n')}\n`;
        exportText('训练报告-' + identity(item) + '.md', report, 'text/markdown');
      }));
      if (status(item) === 'completed' && canTrial(item)) node.append(predictionCard(item));
      const artifacts = item.artifacts || (identity(item) === state.jobId ? job?.artifacts : []);
      for (const artifact of Array.isArray(artifacts) ? artifacts : []) {
        const path = typeof artifact === 'string' ? artifact : artifact.path || artifact.name; if (!path) continue;
        const group = el('details', 'training-details'), destination = input('当前工作目录内的目标路径', path.split(/[\\/]/).at(-1));
        group.append(el('summary', '', artifact.title || path.split(/[\\/]/).at(-1)), el('p', 'training-artifact', path), labelled('导出位置（不覆盖已有文件）', destination), actions(control('导出到工作目录', async () => {
          if (!destination.value.trim()) throw new Error('请填写当前工作目录内的导出路径。');
          await request('jobs.export', { jobId: identity(item), artifactPath: path, destination: destination.value.trim() }); notice('训练产物已导出，原文件保留。');
        }))); node.append(group);
      }
      createTrainingRecordControls(node, { item, request, control, notice, onUpdate: updated => {
        const metadata = { name: updated.name, notes: updated.notes };
        const current = jobs.find(value => identity(value) === identity(updated)); if (current) Object.assign(current, metadata);
        if (identity(job) === identity(updated)) Object.assign(job, metadata);
        node.querySelector('strong').textContent = metadata.name; node.dataset.search = [metadata.name, metadata.notes, updated.algorithm].join(' ').toLocaleLowerCase(); filter.apply();
      } });
      results.append(node);
    }
    results.append(actions(control('刷新记录', async () => { await refreshLists(); if (state.section === 'results') navigate('results'); }), control('导出当前实验指标 CSV', async () => { if (!state.jobId) throw new Error('先选择训练实验。'); const record = await completeRecord(state.jobId); exportText('训练指标.csv', metricsCsv(record.metrics), 'text/csv'); }))); page.append(results); filter.apply();
  }
  async function refreshLists() { const [d, j] = await Promise.all([request('datasets.list'), request('jobs.list')]); if (disposed) return; datasets = list(d, 'datasets'); jobs = list(j, 'jobs'); listRefreshAt = Date.now(); if (!state.datasetId && datasets.length) state.datasetId = identity(datasets[0]); if (!state.jobId && jobs.length) state.jobId = identity(jobs[0]); save(); updateStartControls(); }
  async function readJob(jobId) {
    const emptyMonitor = !job && state.section === 'monitor' && !initialising;
    const sequence = ++requestSequence; if (activeJobId !== jobId) { activeJobId = jobId; metricCursor = 0; rows = []; seenMetrics = new Set(); job = null; }
    const response = await request('jobs.read', { jobId, afterMetric: metricCursor }); if (disposed || sequence !== requestSequence || state.jobId !== jobId) return;
    job = response.job || response; const received = list(response, 'metrics'); for (const entry of received) { const signature = JSON.stringify(entry); if (!seenMetrics.has(signature)) { seenMetrics.add(signature); rows.push(entry); } }
    metricCursor = response.nextMetric ?? response.metricOffset ?? rows.length; hasMoreMetrics = response.hasMoreMetrics === true; if (Array.isArray(response.artifacts)) job.artifacts = response.artifacts;
    const index = jobs.findIndex(item => identity(item) === jobId); if (index < 0) jobs.unshift(job); else jobs[index] = job;
    connected = true; failures = 0; lastRead = Date.now(); reconnect.hidden = true;
    badge.textContent = statusNames[status(job)] || status(job); badge.dataset.status = status(job); chart?.update(rows, comparison); chart?.setStatus(status(job)); liveProgress?.update(job, { connected, lastRead });
    const summary = page.querySelector('[data-live=summary]'), details = page.querySelector('[data-live=details]'); if (summary) renderSummary(summary); if (details) renderRunDetails(details); updateJobControls();
    const resultNode = [...page.querySelectorAll('[data-job-id]')].find(n => n.dataset.jobId === jobId);
    if (resultNode) { resultNode.dataset.status = status(job); resultNode.querySelector('.training-result-state').textContent = `${backendNames[job.backend] || job.backend || ''} · ${algorithmLabels[job.algorithm] || job.algorithm || ''} · ${statusNames[status(job)] || status(job)}`; if (status(job) === 'completed' && canTrial(job) && !resultNode.querySelector('.training-prediction')) resultNode.append(predictionCard(job)); }
    if (emptyMonitor && state.section === 'monitor') navigate('monitor');
    resultFilter?.apply();
  }
  function schedule() { clearTimeout(timer); if (!disposed && active) timer = setTimeout(poll, failures ? Math.min(10000, 1000 * 2 ** failures) : hasMoreMetrics ? 100 : ['running', 'queued', 'preparing', 'pausing', 'stopping'].includes(status(job)) ? 1000 : 5000); }
  async function poll() {
    if (disposed || !active || pollBusy || initialising) return; if (commandPending || starting) { schedule(); return; }
    pollBusy = true;
    try { if (Date.now() - listRefreshAt > 15000) await refreshLists(); if (state.jobId) await readJob(state.jobId); }
    catch (error) { if (!disposed && error?.name !== 'AbortError') { connected = false; failures++; reconnect.hidden = false; badge.textContent = '连接暂不可用'; badge.dataset.status = 'disconnected'; chart?.setStatus('disconnected'); liveProgress?.update(job, { connected, lastRead }); updateJobControls(); if (failures === 1) notice(error.message || '暂时无法读取训练状态，已有曲线保留。', true); } }
    finally { pollBusy = false; schedule(); }
  }
  navigate(state.section);
  async function initialise() {
    initialising = true;
    try {
      const [capabilityResult, listResult] = await Promise.allSettled([request('capabilities'), refreshLists()]); if (disposed) return;
      if (capabilityResult.status === 'fulfilled') capabilities = capabilityResult.value; else throw capabilityResult.reason;
      const available = backendItems().find(b => b.available !== false); if (!backendItems().some(b => b.id === state.backend)) state.backend = available?.id || 'builtin';
      state.parameters = { ...defaultParameters(state.algorithm), ...state.parameters }; save();
      if (listResult.status === 'rejected') throw listResult.reason;
      if (state.jobId) await readJob(state.jobId); else { badge.textContent = available ? '可以配置训练' : '训练环境未就绪'; connected = true; reconnect.hidden = true; }
    } catch (error) { if (!disposed) { connected = false; reconnect.hidden = false; badge.textContent = '暂未连接训练服务'; notice(error.message || '暂时无法读取训练能力。', true); } }
    finally { initialising = false; if (!disposed) navigate(state.section); updateActivity(); }
    if (!disposed && state.compareJobId) void readComparison(state.compareJobId);
    schedule();
  }
  void initialise();
  return { getState: () => ({ ...state }), setActive(value) { active = value; for (const view of pageViews) view.setActive?.(value); if (value) { clearTimeout(timer); void poll(); } else clearTimeout(timer); }, dispose() { if (disposed) return; disposed = true; requestSequence++; clearTimeout(timer); resize.disconnect(); chart?.dispose(); disposePage(); choices.dispose(); feedback.dispose(); root.remove(); } };
}
