import { element as el } from './preview-ui.js';

const stages = {
  idle: '尚未准备', queued: '等待准备', 'waiting-resources': '等待后台资源',
  'downloading-python': '下载 Python', 'downloading-installer': '下载安装器',
  'creating-environment': '创建独立环境', 'installing-packages': '安装训练依赖',
  checking: '检查环境', ready: '已就绪', cancelled: '已取消', interrupted: '已中断', failed: '准备失败',
};
const working = value => ['queued', 'preparing', 'cancelling'].includes(value?.state);
const size = bytes => Number.isFinite(bytes) ? `${(bytes / 1048576).toFixed(bytes < 10485760 ? 1 : 0)} MB` : '';

export function createTrainingEnvironment(host, { request, control, notice, backend, onReady }) {
  let disposed = false, active = true, timer = null, loading = false, last = null, notified = '';
  const card = el('section', 'training-environment'), top = el('div', 'training-source-summary');
  const label = el('strong', '', '训练环境'), status = el('span', 'training-help', '读取中…');
  status.setAttribute('role', 'status'); status.setAttribute('aria-live', 'polite');
  top.append(label, status);
  const progress = el('progress', 'training-environment-progress'); progress.hidden = true;
  progress.setAttribute('aria-label', '环境下载进度');
  const detail = el('small', 'training-help'), message = el('p', 'training-help');
  const actions = el('div', 'training-actions');
  const prepare = control('准备环境', async () => {
    const result = await request('environment.prepare', { backend: typeof backend === 'function' ? backend() : backend || 'builtin', device: 'auto' });
    if (disposed) return;
    render({ ...(last || {}), ...result }); schedule();
  }, true);
  const cancel = control('取消准备', async () => {
    const result = await request('environment.cancel');
    if (disposed) return;
    render(result); schedule();
  }); cancel.hidden = true;
  const retryRead = control('刷新状态', refresh);
  actions.append(prepare, cancel, retryRead);
  const devices = el('details', 'preview-disclosure'), devicesTitle = el('summary', '', '设备与安装记录');
  const hardware = el('div', 'training-environment-hardware'), source = el('p', 'training-help');
  const log = el('pre', 'training-environment-log'); log.setAttribute('aria-label', '环境准备记录');
  devices.append(devicesTitle, hardware, source, log);
  card.append(top, progress, detail, message, actions, devices); host.append(card);

  function render(value) {
    const wasWorking = working(last?.setup);
    last = value;
    const setup = value.setup || {}, busy = working(setup);
    status.textContent = setup.state === 'cancelling' ? '正在取消' : stages[setup.stage] || stages[setup.state] || '待准备';
    if (setup.state === 'idle' && value.pythonPath) status.textContent = '已选择 Python';
    status.dataset.state = setup.state || 'idle';
    prepare.disabled = busy; cancel.hidden = !busy; cancel.disabled = setup.state === 'cancelling';
    prepare.dataset.locked = String(busy); cancel.dataset.locked = String(setup.state === 'cancelling');
    progress.hidden = !busy;
    if (busy && Number.isFinite(setup.totalBytes) && setup.totalBytes > 0) {
      progress.max = setup.totalBytes; progress.value = Math.min(setup.downloadedBytes || 0, setup.totalBytes);
    } else progress.removeAttribute('value');
    detail.textContent = busy && Number.isFinite(setup.downloadedBytes) ? `${size(setup.downloadedBytes)}${Number.isFinite(setup.totalBytes) ? ` / ${size(setup.totalBytes)}` : ''}` : '';
    message.className = ['failed', 'interrupted'].includes(setup.state) ? 'training-field-error' : 'training-help';
    message.textContent = setup.message || '准备过程在后台进行。';
    const device = value.device || {}, snapshot = device.resources?.hardware || device.resources?.snapshot?.hardware || {};
    const memory = snapshot.memory?.totalBytes, cores = device.logicalCores;
    hardware.replaceChildren(el('p', 'training-help', `${cores ? `${cores} 逻辑核心` : 'CPU'}${memory ? ` · ${(memory / 1073741824).toFixed(0)} GB 内存` : ''}`));
    for (const gpu of device.gpus || []) hardware.append(el('p', 'training-help', `${gpu.name}${gpu.memoryBytes ? ` · ${(gpu.memoryBytes / 1073741824).toFixed(1)} GB 显存` : ''}`));
    if (setup.state === 'ready' && setup.probe) hardware.append(el('p', 'training-help', setup.probe.cudaAvailable ? 'CUDA 检查通过' : 'CPU 环境已就绪'));
    const path = setup.pythonPath || value.pythonPath;
    if (path) hardware.append(el('p', 'training-help', path));
    source.textContent = setup.source ? `来源：${setup.source}` : '来源：Astral、PyPI、PyTorch';
    const bottom = log.scrollHeight - log.scrollTop - log.clientHeight < 40;
    log.textContent = (setup.logs || []).slice(-48).join('\n');
    log.hidden = !log.textContent;
    if (bottom) log.scrollTop = log.scrollHeight;
    if (wasWorking && setup.state === 'ready' && setup.id && notified !== setup.id) {
      notified = setup.id;
      Promise.resolve(onReady?.()).catch(error => !disposed && notice?.(error.message || '环境已就绪，请刷新模型能力。', true));
    }
  }
  function schedule() {
    clearTimeout(timer); timer = null;
    if (!disposed && active && working(last?.setup)) timer = setTimeout(refresh, 1500);
  }
  async function refresh() {
    if (disposed || loading || !active) return;
    loading = true;
    try { const result = await request('environment.read'); if (!disposed) render(result); }
    catch (error) { if (!disposed && error?.name !== 'AbortError') { status.textContent = '状态暂不可用'; notice?.(error.message || '环境状态读取失败。', true); } }
    finally { loading = false; schedule(); }
  }
  refresh();
  return {
    refresh,
    setActive(value) { active = Boolean(value); clearTimeout(timer); timer = null; if (active) refresh(); },
    dispose() { disposed = true; clearTimeout(timer); timer = null; },
  };
}
