import { createHash } from 'node:crypto';
import { canRunInParallel } from './tool-scheduling.mjs';
import { isDesktopObservation, isObservationFailure } from './tool-outcomes.mjs';

const hash = text => createHash('sha256').update(text).digest('hex');
function stable(value, depth = 0) {
  if (depth > 64) throw new Error('Observation nesting limit');
  if (Array.isArray(value)) return value.map(item => stable(item, depth + 1));
  if (!value || typeof value !== 'object') return value;
  return Object.fromEntries(Object.keys(value).sort().map(key => [key, stable(value[key], depth + 1)]));
}

export function observationFingerprint(value) {
  try { return hash(JSON.stringify(stable(value))); } catch { return null; }
}

function callKey(call) {
  const input = call.name.startsWith('mcp.') ? call.arguments?.arguments : call.arguments;
  return observationFingerprint([call.name, input]);
}

/**
 * Only known stateless web operations may reuse an observation; local reads always recheck the source.
 * 只有已知无状态网页操作可复用观察，本机读取始终重新检查源内容。
 */
export const canReuseObservation = call => call?.name?.startsWith('mcp.') === true && canRunInParallel(call);

/**
 * Owned by one immutable request context, never shared between requests or persisted.
 * 缓存仅属于单个不可变请求上下文，不跨请求共享，也不持久化。
 */
export class RequestObservationCache {
  constructor({ now = () => performance.now(), ttlMs = 15000 } = {}) {
    this.now = now; this.ttlMs = ttlMs; this.entries = new Map(); this.bytes = 0;
  }
  clear() { this.entries.clear(); this.bytes = 0; }
  _delete(key) { const entry = this.entries.get(key); if (entry) this.bytes -= entry.bytes; this.entries.delete(key); }
  get(call, scope = null) {
    if (!canReuseObservation(call)) return null;
    const key = callKey(call), entry = this.entries.get(key);
    if (!entry) return null;
    if (this.now() - entry.time >= this.ttlMs || entry.scope !== scope) { this._delete(key); return null; }
    return { result: structuredClone(entry.result), capturedAt: entry.capturedAt };
  }
  remember(call, result, scope = null) {
    if (!canReuseObservation(call) || result.isError || result.code || result.canonical?.isError ||
        !result.canonical || typeof result.content !== 'string' || result.content.length > 60000 ||
        (result.canonical.content ?? []).some(block => !['text', 'resource_link'].includes(block.type))) return;
    const key = callKey(call);
    if (!key) return;
    let serialized;
    try { serialized = JSON.stringify(result); } catch { return; }
    const bytes = Buffer.byteLength(serialized);
    if (bytes > 65536) return;
    for (const [id, entry] of this.entries) if (this.now() - entry.time >= this.ttlMs) this._delete(id);
    this._delete(key);
    while (this.entries.size && (this.entries.size >= 32 || this.bytes + bytes > 262144)) this._delete(this.entries.keys().next().value);
    this.entries.set(key, { result: JSON.parse(serialized), time: this.now(), capturedAt: new Date().toISOString(), bytes, scope });
    this.bytes += bytes;
  }
}

/**
 * Repeated successful observations, not repeated call IDs or prose, determine lack of progress.
 * 以重复成功观察判断缺乏进展，不能仅凭重复调用 ID 或正文判断。
 */
export class ToolProgressGuard {
  constructor() { this.observations = new Map(); this.stagnantRounds = 0; }
  observeRound(pairs) {
    let repeated = pairs.length > 0;
    const observations = new Map();
    for (const { call, result } of pairs) {
      if (!canRunInParallel(call) || result.isError || result.code ||
          (result.status && result.status !== 'completed')) {
        this.observations.clear(); this.stagnantRounds = 0;
        return { repeated: false, warning: false, finalize: false };
      }
      const key = callKey(call);
      const fingerprint = result.observationHash ?? result.resultRef?.sha256 ?? observationFingerprint(result.content);
      if (!key || !fingerprint || this.observations.get(key) !== fingerprint) repeated = false;
      if (key && fingerprint) observations.set(key, fingerprint);
    }
    for (const [key, fingerprint] of observations) {
      this.observations.delete(key); this.observations.set(key, fingerprint);
      while (this.observations.size > 256) this.observations.delete(this.observations.keys().next().value);
    }
    this.stagnantRounds = repeated ? this.stagnantRounds + 1 : 0;
    return { repeated, warning: this.stagnantRounds === 2, finalize: this.stagnantRounds >= 3 };
  }
}

/**
 * Three failed attempts at the same read target end retries, not the conversation.
 * 同一读取目标连续失败三次后停止重试，但不终止整个聊天。
 */
export class ToolReadFailureGuard {
  constructor() { this.previousTargets = null; this.failedRounds = 0; }
  observeRound(pairs) {
    if (!pairs.length || pairs.some(({ call, result }) => !isObservationFailure(call, result))) {
      this.previousTargets = null; this.failedRounds = 0;
      return { repeated: false, warning: false, finalize: false };
    }
    const targets = pairs.map(({ call, result }) => {
      const input = call.name.startsWith('mcp.') ? call.arguments?.arguments : call.arguments;
      const target = isDesktopObservation(call.name)
        ? [input?.windowId, input?.processId, input?.elementId, input?.region, input?.crop]
        : [result.browser.connectionId, result.browser.tabId, input?.url, input?.uid, input?.ref, input?.selector];
      return observationFingerprint([call.name, target]);
    }).sort().join(':');
    const repeated = targets === this.previousTargets;
    this.failedRounds = repeated ? this.failedRounds + 1 : 1;
    this.previousTargets = targets;
    return { repeated, warning: this.failedRounds === 2, finalize: this.failedRounds >= 3 };
  }
}
