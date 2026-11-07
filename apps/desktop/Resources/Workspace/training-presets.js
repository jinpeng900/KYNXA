import { element as el } from './preview-ui.js';

export function trainingPreset(algorithm, defaults, kind) {
  const result = structuredClone(defaults);
  if (kind === 'default') return result;
  if (algorithm.startsWith('text.') || algorithm.startsWith('vlm.')) Object.assign(result, { batchSize: 1, maxLength: 256, gradientAccumulationSteps: 4, gradientCheckpointing: true });
  else if (algorithm === 'vision.classifier') Object.assign(result, { hiddenLayers: [8, 16], batchSize: 8, imageSize: 48 });
  else if (algorithm.startsWith('mlp.')) Object.assign(result, { hiddenLayers: [16, 8], batchSize: 8 });
  else if (/forest|boost/.test(algorithm)) Object.assign(result, { estimators: 50, maxDepth: 6 });
  result.cpuThreads = 2;
  return result;
}
export function createTrainingPresets(parent, { control, apply, estimate }) {
  const row = el('div', 'training-actions workbench-row training-presets');
  row.append(control('常规起点', () => apply('default')), control('轻量起点', () => apply('light')));
  const output = el('p', 'training-help training-estimate'); output.setAttribute('role', 'status'); output.hidden = true;
  row.append(control('估算资源', async () => {
    const result = await estimate(); if (!output.isConnected) return; const plan = result.plan;
    const size = value => value >= 1073741824 ? (value / 1073741824).toFixed(1) + ' GiB' : Math.ceil(value / 1048576) + ' MiB';
    output.textContent = `预计内存 ${size(plan.memoryBytes)}${plan.gpuMemoryBytes ? ' · 显存 ' + size(plan.gpuMemoryBytes) : ''} · ${plan.cpuThreads} 线程${result.devicePending ? '（暂按 CPU 估算）' : ''}，实际以启动检查为准。`; output.hidden = false;
  })); parent.append(row, output);
}
