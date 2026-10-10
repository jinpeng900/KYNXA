import { createHash } from 'node:crypto';
import { canRunInParallel } from '../tools/tool-scheduling.mjs';
import { ToolCallDecodeFailure } from '../models/tool-call-validation.mjs';

const observationNames = new Set(['tool.search', 'tool.load', 'skill.list', 'skill.read', 'skill.inspect',
  'skill.check', 'skill.resource.read', 'computer.apps', 'computer.windows', 'computer.read',
  'computer.screenshot', 'terminal.host.read']);

export function isRecoveryObservation(call) {
  return observationNames.has(call.name) || canRunInParallel(call);
}

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort().map(key => [key, canonical(value[key])]));
  return value;
}

function effectKey(call) {
  const { reason, policy, ...business } = call.arguments;
  // App-owned justifications may change during repair; third-party business fields remain intact.
  // 恢复时应用自有理由可能变化，第三方 arguments 内的业务字段仍完整参与动作身份。
  return createHash('sha256').update(JSON.stringify([call.name, canonical(business)])).digest('hex');
}

/** Receipts are scoped to this request. Decoder repair never replays a successful or uncertain effect.
 * 回执只属于当前请求；修复模型步骤时不重放已成功或结果未知的副作用。 */
export class ToolRecoveryLedger {
  constructor() {
    this.completed = new Map(); this.protectedEffects = null; this.unknown = new Map(); this.receipts = []; this.events = [];
    this.verifiedEffects = [];
  }

  observe(call, result) {
    if (isRecoveryObservation(call)) return;
    const key = effectKey(call);
    this.receipts.push({ id: call.id, name: call.name, status: result.status ?? (result.isError ? 'error' : 'completed') });
    if (result.status === 'unknown') this.unknown.set(key, { call, result });
    else if (!result.isError && (!result.status || result.status === 'completed')) this.completed.set(key, { call, result });
  }

  protectCompletedEffects({ includeCurrent = false } = {}) {
    // Freeze only pre-failure effects. Later observations cannot expire protection or deduplicate new user work.
    // 只冻结故障前的成功副作用；后续读取不能解除保护，也不把新工作扩大成自动去重范围。
    this.protectedEffects ??= new Map(this.completed);
    // A later context rejection must also protect effects completed since the first decoder repair.
    // 后续上下文拒绝还须保护首次解码修复之后完成的副作用。
    if (includeCurrent) for (const [key, receipt] of this.completed) this.protectedEffects.set(key, receipt);
  }

  previous(call) { return isRecoveryObservation(call) ? null : this.protectedEffects?.get(effectKey(call)); }
  get hasUnknownEffects() { return this.unknown.size > 0; }

  resolveVerified(proofs = []) {
    // Only broker-owned state checks supply proofs; model text never certifies an uncertain dispatch.
    // 仅权限代理持有的真实状态核验提供证明；模型文字不能自行确认未知派发。
    let resolved = 0;
    for (const proof of proofs) {
      const record = [...this.unknown.entries()].find(([, item]) => item.call.id === proof.toolCallId);
      if (!record || proof.outcome !== 'dispatch-confirmed' || !proof.result || proof.result.isError ||
          proof.result.status !== 'completed') continue;
      const [key, original] = record;
      const verified = { call: original.call, result: proof.result };
      this.unknown.delete(key); this.completed.set(key, verified);
      this.protectedEffects ??= new Map(this.completed);
      this.protectedEffects.set(key, verified);
      this.verifiedEffects.push({ toolCallId: proof.toolCallId, observationToolCallId: proof.observationToolCallId,
        outcome: proof.outcome, ...(proof.result.resultRef ? { resultRef: proof.result.resultRef } : {}) });
      resolved++;
    }
    return resolved;
  }

  classify(error) {
    return error instanceof ToolCallDecodeFailure && error.executed === false ? 'repair-unexecuted-model-step' : 'stop';
  }

  record(code, action, round) { this.events.push({ code, action, round, dispatched: false }); this.events = this.events.slice(-8); }
  audit() { return { version: 1, events: [...this.events], unknownEffects: this.unknown.size,
    verifiedEffects: [...this.verifiedEffects] }; }

  fallback(message = '') {
    const chinese = /\p{Script=Han}/u.test(message);
    const counts = { completed: 0, unknown: 0, error: 0 };
    const verifiedIds = new Set(this.verifiedEffects.map(item => item.toolCallId));
    for (const receipt of this.receipts) {
      if (receipt.status === 'completed' || verifiedIds.has(receipt.id)) counts.completed++;
      else if (receipt.status === 'unknown') counts.unknown++;
      else counts.error++;
    }
    return chinese
      ? `已有执行记录已保留：已完成 ${counts.completed} 项，结果尚未确认 ${counts.unknown} 项，未成功 ${counts.error} 项。后续工具调用未能可靠恢复，任务尚未全部完成；没有重做已完成操作或结果未知的操作。你可以继续对话。`
      : `Saved execution records: ${counts.completed} completed, ${counts.unknown} unconfirmed, ${counts.error} unsuccessful. Further tool calls could not be recovered reliably, so the task remains incomplete. Completed and unconfirmed operations were not replayed. You can continue the conversation.`;
  }
}
