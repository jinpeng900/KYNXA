import { createHash } from 'node:crypto';
import { isAbsolute, normalize } from 'node:path';

const READ_TOOLS = new Set(['filesystem.read', 'filesystem.stat', 'knowledge.read']);
const READ_NAVIGATION_FIELDS = new Set(['sourceRef', 'modelSourceRef', 'canonicalSourceRef', 'evidenceDecision']);
const DIGEST = /^[a-f0-9]{64}$/iu;
const digest = value => createHash('sha256').update(value).digest('hex');
const validOffset = value => Number.isSafeInteger(value) && value >= 0;

export function isCompactionReadTool(name) { return READ_TOOLS.has(name); }

/** Describe only explicit returned identities and versions; call arguments never establish freshness.
 * 仅描述结果实际返回的目标身份与版本；调用参数不能证明来源新鲜度。 */
export function describeToolObservation({ name, status, payload, scopeKey, archiveVerified = false }) {
  if (!READ_TOOLS.has(name) || status !== 'completed' || typeof scopeKey !== 'string' || !scopeKey) return null;
  let canonical;
  try { canonical = typeof payload === 'string' ? JSON.parse(payload) : payload; } catch { return null; }
  if (!canonical || typeof canonical !== 'object' || Array.isArray(canonical) || canonical.isError === true) return null;
  const value = canonical.structuredContent ?? canonical;
  if (!value || typeof value !== 'object' || Array.isArray(value) || value.storageError || value.contextCompacted ||
      value.resultUnavailable || value.truncated === true && typeof value.preview === 'string') return null;
  let target, version, page = 'stat';
  if (name.startsWith('filesystem.')) {
    if (typeof value.path !== 'string' || !isAbsolute(value.path) || !DIGEST.test(value.sha256 ?? '')) return null;
    target = { path: normalize(value.path) }; version = { sha256: value.sha256 };
  } else {
    if (typeof value.sourceId !== 'string' || !value.sourceId || typeof value.scopeKey !== 'string' || !value.scopeKey ||
        !DIGEST.test(value.contentHash ?? '') || !(typeof value.sourceRevision === 'string' && value.sourceRevision ||
        Number.isSafeInteger(value.sourceRevision) && value.sourceRevision >= 0)) return null;
    target = { sourceId: value.sourceId, scopeKey: value.scopeKey };
    version = { contentHash: value.contentHash, sourceRevision: value.sourceRevision };
  }
  if (name !== 'filesystem.stat') {
    const text = name === 'knowledge.read' ? value.text : value.content;
    if (typeof text !== 'string' || !validOffset(value.offset) || !validOffset(value.nextOffset) ||
        value.nextOffset !== value.offset + text.length || typeof value.hasMore !== 'boolean') return null;
    // A finished section page may still cover only the first chapter of the full source.
    // 小节分页结束仍可能只覆盖来源首章，必须核对全文长度才能视为完整来源。
    page = !value.hasMore && value.offset === 0 && validOffset(value.totalCharacters) &&
      value.nextOffset === value.totalCharacters ? 'whole' : JSON.stringify([value.offset, value.nextOffset]);
  }
  // Read handles and runtime follow-up hints navigate the same fact; retain all source/range/content metadata.
  // 读取句柄与运行时续读提示只导航同一事实，来源、范围、正文及其他元信息全部参与同文校验。
  const observed = name === 'knowledge.read' ? Object.fromEntries(Object.entries(value)
    .filter(([key]) => !READ_NAVIGATION_FIELDS.has(key))) : value;
  return { target, version, page, archiveVerified,
    targetKey: JSON.stringify([scopeKey, name, target, page]), contentFingerprint: digest(JSON.stringify(observed)) };
}

/** A later verified observation can replace only an earlier exact duplicate or explicit changed version.
 * 后续已核验观察只替代先前同文观察或明确变化版本，时间与轮次数本身不是过时证据。 */
export function planObservationCompaction(observations) {
  const latest = new Map(), plans = [];
  for (const source of observations) if (source.identity?.archiveVerified === true && source.resultRef)
    latest.set(source.identity.targetKey, source);
  for (const source of observations) {
    if (source.identity?.archiveVerified !== true || !source.resultRef) continue;
    const replacement = latest.get(source.identity.targetKey);
    if (!replacement || replacement === source) continue;
    const duplicate = source.identity.contentFingerprint === replacement.identity.contentFingerprint;
    const versionChanged = JSON.stringify(source.identity.version) !== JSON.stringify(replacement.identity.version);
    if (duplicate || versionChanged) plans.push({ source, replacement, reason: duplicate ? 'duplicate-observation' : 'superseded-version' });
  }
  return plans;
}

export function observationCompactionText({ source, replacement, reason }) {
  return JSON.stringify({ contextCompacted: true, observationCompacted: true, reason,
    toolCallId: source.callId, tool: source.name, status: source.status, target: source.identity.target,
    observedVersion: source.identity.version, page: source.identity.page, originalCharacters: source.originalCharacters,
    resultRef: source.resultRef,
    replacedBy: { toolCallId: replacement.callId, resultRef: replacement.resultRef, observedVersion: replacement.identity.version },
    navigation: { tool: 'tool.result.read', arguments: { id: source.resultRef.id, offset: 0, limit: 4096 } },
    notice: 'Earlier archived read observation; its receipt remains recorded. Use the later verified observation or read this saved result for the old version. Never repeat a completed effect to recover context.' });
}
