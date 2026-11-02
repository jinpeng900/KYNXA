import { element as el, input, field } from './preview-ui.js';

export function createTrainingRecordControls(parent, { item, request, control, onUpdate, notice }) {
  const group = el('details', 'training-details training-record-editor'); group.append(el('summary', '', '名称、备注与整理'));
  const name = input('', item.name || ''), notes = el('textarea'); name.maxLength = 160; notes.maxLength = 2000; notes.value = item.notes || ''; notes.rows = 3;
  name.setAttribute('aria-label', '实验名称'); notes.setAttribute('aria-label', '实验备注');
  const cleanup = el('div', 'training-cleanup'), actions = el('div', 'training-actions workbench-row');
  actions.append(control('保存备注', async () => { const result = await request('jobs.update', { jobId: item.id, name: name.value.trim(), notes: notes.value.trim() }); Object.assign(item, result.metadata); onUpdate?.(item); notice?.('实验信息已保存。'); }));
  actions.append(control('查看可清理检查点', async () => {
    const plan = await request('jobs.cleanup.preview', { jobId: item.id }); if (!group.isConnected) return;
    cleanup.replaceChildren(el('p', 'training-help', plan.count ? `${plan.count} 份旧检查点 · ${(plan.bytes / 1048576).toFixed(1)} MiB；保留续训断点与模型产物。` : '没有可清理的旧检查点。'));
    if (plan.count) cleanup.append(control('清理这些旧检查点', async () => { const result = await request('jobs.cleanup.apply', { jobId: item.id, token: plan.token }); if (group.isConnected) cleanup.replaceChildren(el('p', 'training-help', `已清理 ${result.removedCount} 份旧检查点。`)); }));
  }));
  group.append(field('实验名称', name), field('备注', notes), actions, cleanup); parent.append(group);
}

export function createTrainingExperimentFilter(parent, { select, state, onState }) {
  const row = el('div', 'training-experiment-filter'), search = input('搜索实验名称、备注或算法', state.experimentSearch || ''); search.setAttribute('aria-label', '搜索实验');
  const apply = () => {
    const query = search.value.trim().toLocaleLowerCase(), filter = state.experimentStatus || '';
    parent.querySelectorAll('[data-job-id]').forEach(node => { node.hidden = !!(query && !node.dataset.search.includes(query) || filter && node.dataset.status !== filter); });
  };
  search.oninput = () => { state.experimentSearch = search.value; onState?.(); apply(); };
  const status = select([['', '全部状态'], ['completed', '已完成'], ['running', '训练中'], ['paused', '已暂停'], ['failed', '失败'], ['stopped', '已停止']], state.experimentStatus || '', value => { state.experimentStatus = value; onState?.(); apply(); });
  status.setAttribute('aria-label', '实验状态筛选'); row.append(search, status); parent.append(row);
  return { apply };
}
