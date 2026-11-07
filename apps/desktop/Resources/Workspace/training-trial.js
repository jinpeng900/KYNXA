import { element as el, button, input, field } from './preview-ui.js';
import { createSelectScope } from './workbench-controls.js';

const finished = state => ['completed', 'failed', 'cancelled', 'interrupted'].includes(state);
const label = mode => mode === 'base' ? '原底座' : '训练产物';
const display = value => typeof value === 'number' && Number.isFinite(value) ? Number(value.toPrecision(5)).toString() : typeof value === 'object' ? JSON.stringify(value) : String(value ?? '—');
function area(value = '', placeholder = '') { const node = el('textarea', 'training-config-editor'); node.value = value; node.placeholder = placeholder; return node; }
function row(...nodes) { const host = el('div', 'training-actions workbench-row'); host.append(...nodes); return host; }
function labelled(name, node) { node.setAttribute('aria-label', name); return field(name, node); }

export function createTrainingTrial(parent, { item, request, onState, state = {}, notice }) {
  const jobId = item.id || item.jobId || item.config?.id;
  const host = el('details', 'training-details training-prediction training-trial'), body = el('div', 'training-trial-body');
  host.append(el('summary', '', '试用模型'), body); parent.append(host);
  const choices = createSelectScope(host);
  let saved = { mode: 'trained', maxNewTokens: 256, messages: [], ...state }, schema, loading = false, disposed = false, active = true, timer, reading = false, readFailures = 0;
  let trial = saved.trial || null, status, output, send, cancel, editor, retryRead;
  const pending = new Set();
  function save(patch = {}) { saved = { ...saved, ...patch }; if (!disposed) onState?.(saved); }
  function report(error) { if (!disposed) notice?.(error?.message || String(error), true); }
  function action(title, run, primary = false) {
    const node = button(title, async () => {
      if (pending.has(node) || disposed) return;
      pending.add(node); node.disabled = true; node.setAttribute('aria-busy', 'true');
      try { await run(); } catch (error) { report(error); }
      finally { pending.delete(node); node.removeAttribute('aria-busy'); node.disabled = false; sync(); }
    }); if (primary) node.classList.add('primary'); return node;
  }
  function sync() {
    if (!send) return;
    const running = !!trial && !finished(trial.state);
    send.disabled = running || pending.has(send) || !schema?.available;
    if (cancel) { cancel.hidden = !running; cancel.disabled = !running || pending.has(cancel); }
    if (editor) editor.disabled = running;
    if (status) status.textContent = trial?.message || '';
    if (retryRead) retryRead.hidden = readFailures === 0;
  }
  function predictionResults(values, node) {
    node.replaceChildren();
    if (!Array.isArray(values) || !values.length) return;
    node.append(el('p', '', `已返回 ${values.length} 条预测结果`));
    const table = el('table'), head = el('thead'), header = el('tr'), tbody = el('tbody');
    ['样本', '预测值', '概率／距离'].forEach(title => header.append(el('th', '', title))); head.append(header); table.append(head, tbody);
    values.forEach((value, index) => {
      const tr = el('tr');
      const detail = Array.isArray(value.probabilities) ? value.probabilities.map(p => `${p.label}：${(p.probability * 100).toFixed(2)}%`).join('；') : Array.isArray(value.squaredDistances) ? value.squaredDistances.map((v, i) => `${i}：${Number(v).toPrecision(4)}`).join('；') : '—';
      tr.append(el('td', '', index + 1), el('td', '', display(value.prediction)), el('td', '', detail)); tbody.append(tr);
    }); node.append(table);
  }
  async function predict(samples) {
    const response = await request('jobs.predict', { jobId, samples });
    if (disposed) return;
    if (!Array.isArray(response.predictions)) throw new Error('预测未返回有效结果。');
    save({ predictions: response.predictions }); predictionResults(response.predictions, output);
  }
  function tabular() {
    const fields = el('div', 'training-fields'), controls = [];
    for (const [index, feature] of schema.fields.entries()) {
      const value = saved.features?.[index] ?? feature.example ?? '';
      const control = input(feature.kind === 'numeric' ? '数值，留空使用训练填充值' : '类别，留空表示缺失', value);
      if (feature.kind === 'numeric') { control.type = 'number'; control.step = 'any'; }
      control.oninput = () => save({ features: controls.map(c => c.value) }); controls.push(control);
      const group = labelled(feature.name, control);
      if (feature.kind !== 'numeric' && feature.options?.length) group.append(el('small', 'training-help', feature.options.slice(0, 6).join(' · ')));
      fields.append(group);
    }
    send = action('运行预测', async () => {
      const features = controls.map((node, index) => {
        if (node.validity.badInput) throw new Error(`请填写有效的${schema.fields[index].name}。`);
        if (!node.value.trim()) return null;
        if (schema.fields[index].kind !== 'numeric') return node.value;
        const value = Number(node.value); if (!Number.isFinite(value)) throw new Error(`请填写有效的${schema.fields[index].name}。`); return value;
      }); await predict([{ features }]);
    }, true);
    const advanced = el('details', 'training-details'), jsonInput = area(saved.json || '', '[{"features":[1,2]},{"features":[3,4]}]');
    advanced.append(el('summary', '', '多样本 JSON'), labelled('多样本 JSON', jsonInput));
    jsonInput.oninput = () => save({ json: jsonInput.value });
    advanced.append(row(action('批量预测', async () => {
      let parsed; try { parsed = JSON.parse(jsonInput.value); } catch { throw new Error('JSON格式不正确，请检查括号和逗号。'); }
      let values = Array.isArray(parsed) ? parsed : parsed?.samples || [parsed];
      if (!Array.isArray(values)) throw new Error('samples需要是样本数组。');
      if (values.length && values.every(value => value === null || typeof value !== 'object')) values = [{ features: values }];
      const samples = values.map(value => Array.isArray(value) ? { features: value } : value);
      if (!samples.length || samples.some(value => !Array.isArray(value?.features))) throw new Error('每条样本需要features数组。');
      await predict(samples);
    })));
    output = el('div', 'training-predictions'); output.setAttribute('aria-live', 'polite');
    body.append(fields, row(send), advanced, output); predictionResults(saved.predictions, output); sync();
  }
  function showReplies() {
    if (!output) return;
    output.replaceChildren();
    for (const turn of saved.turns || []) {
      output.append(el('p', 'training-trial-prompt', turn.prompt));
      for (const result of turn.results || []) {
        const card = el('div', 'training-trial-reply'); card.append(el('strong', '', label(result.mode)), el('p', '', result.reply || '（模型返回空文本）'));
        output.append(card);
      }
    }
    if (trial && !finished(trial.state)) {
      for (const result of trial.results || []) { const card = el('div', 'training-trial-reply'); card.append(el('strong', '', label(result.mode)), el('p', '', result.reply || '（模型返回空文本）')); output.append(card); }
    }
  }
  function accept(response) {
    trial = response.trial; if (!trial?.id) throw new Error('试用记录无效。');
    if (finished(trial.state) && saved.recordedId !== trial.id && trial.results?.length) {
      const turns = [...(saved.turns || []), { prompt: saved.currentPrompt || saved.draft || '', results: trial.results }];
      const result = trial.results.find(v => v.mode === 'trained') || trial.results[0];
      const messages = trial.state === 'completed' && result?.reply ? [...(saved.pendingMessages || []), { role: 'assistant', content: result.reply }] : saved.messages;
      save({ turns, messages, recordedId: trial.id, draft: trial.state === 'completed' ? '' : saved.draft });
      if (editor && trial.state === 'completed') editor.value = '';
    }
    save({ trial, trialId: trial.id }); sync(); showReplies();
  }
  async function poll() {
    clearTimeout(timer);
    if (disposed || !active || !host.open || reading || !saved.trialId || trial && finished(trial.state)) return;
    reading = true;
    try { const response = await request('jobs.trial.read', { jobId, trialId: saved.trialId }); if (!disposed) { readFailures = 0; accept(response); } }
    catch (error) { readFailures++; if (!disposed && status) status.textContent = `状态暂未更新：${error?.message || error}`; if (retryRead) retryRead.hidden = false; }
    finally { reading = false; if (!disposed && active && host.open && (!trial || !finished(trial.state))) timer = setTimeout(poll, Math.min(1000 * 2 ** Math.min(readFailures, 4), 16000)); }
  }
  function textTrial() {
    const mode = choices.select([['trained', '训练产物'], ['base', '原底座'], ['compare', '对比底座与产物']], saved.mode, value => { save({ mode: value, messages: [] }); });
    const length = input('', saved.maxNewTokens); length.type = 'number'; length.min = '1'; length.max = '8192'; length.step = '1';
    length.oninput = () => save({ maxNewTokens: length.value });
    editor = area(saved.draft || '', '输入想试一试的问题'); editor.classList.add('training-trial-question'); editor.oninput = () => save({ draft: editor.value });
    const advanced = el('details', 'training-details'); advanced.append(el('summary', '', '回复设置'), labelled('最多生成 token', length));
    output = el('div', 'training-trial-dialogue');
    status = el('p', 'training-help training-trial-status'); status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
    send = action('发送试聊', async () => {
      const prompt = editor.value.trim(), tokens = Number(length.value);
      if (!prompt) throw new Error('请输入试聊内容。');
      if (!Number.isInteger(tokens) || tokens < 1 || tokens > 8192) throw new Error('回复长度应为1至8192 token。');
      // Comparison uses the same supplied history, and runs twice only when selected.
      const messages = [...(saved.messages || []), { role: 'user', content: prompt }], id = crypto.randomUUID();
      save({ currentPrompt: prompt, pendingMessages: messages, trialId: id });
      trial = { id, state: 'queued', message: '正在准备试用。', results: [] }; save({ trial }); sync();
      try {
        const response = await request('jobs.trial.start', { jobId, trialId: id, messages, mode: saved.mode, maxNewTokens: tokens });
        if (!disposed) { accept(response); poll(); }
      } catch (error) {
        // A lost acceptance reply is resolved by ID; never silently duplicate inference.
        try { const response = await request('jobs.trial.read', { jobId, trialId: id }); if (!disposed) { accept(response); poll(); } }
        catch (readError) {
          if (readError?.code === 'TRAINING_NOT_FOUND' || String(readError?.message).includes('训练记录不存在')) {
            trial = null; save({ trial, trialId: null }); sync(); throw error;
          }
          trial.message = '正在重新读取本次试用状态。'; save({ trial }); sync(); poll();
        }
      }
    }, true);
    cancel = action('取消试用', async () => { const response = await request('jobs.trial.cancel', { jobId, trialId: saved.trialId }); if (!disposed) { accept(response); poll(); } });
    retryRead = action('刷新状态', poll); retryRead.hidden = true;
    const clear = action('新试聊', () => { if (trial && !finished(trial.state)) throw new Error('请先取消正在进行的试用。'); trial = null; save({ messages: [], turns: [], trial: null, trialId: null, recordedId: null }); showReplies(); sync(); });
    body.append(labelled('试用方式', mode), output, labelled('试聊输入', editor), advanced, row(send, cancel, clear, retryRead), status);
    showReplies(); sync(); poll();
  }
  async function load() {
    if (disposed || loading || schema) return;
    loading = true; body.replaceChildren(el('p', 'training-help', '正在读取模型输入…'));
    try {
      const response = await request('jobs.predict.schema', { jobId }); if (disposed) return; schema = response; body.replaceChildren();
      if (schema.kind === 'text') textTrial(); else if (schema.kind === 'tabular' && Array.isArray(schema.fields)) tabular(); else throw new Error('这个产物暂不支持直接试用。');
      if (!schema.available) body.prepend(el('p', 'training-field-error', schema.reason || '请先在训练台准备运行环境。'));
    } catch (error) { if (!disposed) { schema = null; body.replaceChildren(el('p', 'training-field-error', error?.message || String(error)), action('重新读取', load)); } }
    finally { loading = false; }
  }
  host.ontoggle = () => { save({ open: host.open }); if (host.open) { load(); poll(); } else clearTimeout(timer); };
  host.open = !!saved.open;
  if (host.open) load();
  return { element: host, setActive(value) { active = value; if (active && host.open) { load(); poll(); } else clearTimeout(timer); }, dispose() { disposed = true; clearTimeout(timer); choices.dispose(); host.ontoggle = null; } };
}
