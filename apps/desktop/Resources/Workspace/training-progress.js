import { element as el } from './preview-ui.js';

const routineMessages = new Set(['正在训练', '正在训练图像分类网络', '正在微调本地模型', '正在聚类', '训练完成，模型与评估结果已保存', '图像训练完成，模型与评估结果已保存', '聚类完成，模型已保存', '微调完成，结果已独立保存']);

export const statusNames = { created: '待启动', ready: '已准备', queued: '等待资源', preparing: '准备中', running: '训练中', pausing: '正在保存', paused: '已暂停', stopping: '正在停止', stopped: '已停止', completed: '已完成', failed: '训练失败', interrupted: '已中断' };
export function trainingStatus(job) { return job?.state?.status || job?.status || (typeof job?.state === 'string' ? job.state : '') || 'created'; }
export function duration(value) {
  if (!Number.isFinite(value) || value < 0) return '—';
  if (value > 0 && value < 1) return '不到1秒';
  const seconds = Math.floor(value), minutes = Math.floor(seconds / 60), hours = Math.floor(minutes / 60);
  return hours ? `${hours}小时 ${minutes % 60}分` : minutes ? `${minutes}分 ${seconds % 60}秒` : `${seconds}秒`;
}
export function latestMetric(rows, keys) {
  for (let i = rows.length - 1; i >= 0; i--) for (const key of keys) if (Number.isFinite(rows[i][key])) return rows[i][key];
}

// Status uses worker facts; disconnection does not invent a stopped job. / 状态来自训练器，断线不推断训练已停止。
export function createTrainingProgress(host) {
  const panel = el('div', 'training-progress'), line = el('div', 'training-progress-line'), title = el('strong'), fraction = el('span');
  const bar = el('progress'), message = el('p', 'training-run-message'), freshness = el('span', 'training-freshness');
  bar.max = 1; bar.setAttribute('aria-label', '训练进度'); line.append(title, fraction); panel.append(line, bar, message, freshness); host.append(panel);
  let previous = '';
  return { update(job, { connected = true, lastRead = 0 } = {}) {
    const current = typeof job?.state === 'object' ? job.state : job || {}, code = trainingStatus(job);
    const progress = typeof current.progress === 'number' ? current.progress : current.progress?.fraction;
    const reason = current.error?.message || current.message || job?.message || job?.reason;
    const hints = { queued: '资源可用后自动开始。', preparing: '正在准备资料和模型。', paused: '进度已保存。', stopped: '已有结果已保存。', failed: '训练失败，请检查运行信息。' };
    const state = JSON.stringify([code, progress, reason, connected, lastRead]); if (state === previous) return; previous = state;
    panel.dataset.status = connected ? code : 'disconnected'; title.textContent = connected ? statusNames[code] || code : '连接暂不可用';
    const known = Number.isFinite(progress); bar.hidden = !job;
    if (known) { bar.value = Math.max(0, Math.min(1, progress)); fraction.textContent = `${Math.round(bar.value * 100)}%`; }
    else { bar.removeAttribute('value'); fraction.textContent = ''; }
    const redundant = ['running', 'completed'].includes(code) && !current.error && routineMessages.has(String(reason));
    message.textContent = reason && !redundant ? String(reason) : job ? hints[code] || '' : ''; message.hidden = !message.textContent;
    freshness.textContent = connected ? '' : '正在重连，已有进度保留。'; freshness.hidden = connected;
    panel.title = lastRead ? `上次同步 ${new Date(lastRead).toLocaleTimeString('zh-CN', { hour12: false })}` : '';
  } };
}
