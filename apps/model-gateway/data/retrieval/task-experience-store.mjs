import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { ensureLocalDirectory, inspectLocalPath } from '../../platform/tool-paths.mjs';
import { retrievalScopeKeys, retrievalFailure } from './retrieval-contracts.mjs';

const MAX_EXPERIENCE_AGE_MS = 30 * 24 * 60 * 60 * 1000;
const MAX_EXPERIENCE_FILE_BYTES = 2 * 1024 * 1024;
const MAX_EXPERIENCE_ENTRIES = 64;
const validText = (value, maximum) => typeof value === 'string' && value.length > 0 && value.length <= maximum && !value.includes('\0');
const validSourceRefs = value => Array.isArray(value) && value.length <= 64 &&
  value.every(reference => validText(reference, 16384) && reference.startsWith('rag1:'));
const validNavigation = value => Array.isArray(value) && value.length <= 32 &&
  value.every(step => validText(step, 1000));

/** Store version-bound navigation experience, never a replacement answer or a permission grant.
 * 保存版本绑定的检索路径经验，不把历史结论当现时答案，也不能以经验授予权限。 */
export class TaskExperienceStore {
  constructor(root, { clock = Date.now } = {}) {
    this.directory = join(root, 'Retrieval', 'Experiences'); this.queue = Promise.resolve(); this.clock = clock;
    this.readDiagnostics = new Map();
  }
  _run(operation) { const pending = this.queue.catch(() => {}).then(operation); this.queue = pending; return pending; }
  _path(scopeKey) { return join(this.directory, `${createHash('sha256').update(scopeKey).digest('hex')}.json`); }
  async _read(scopeKey) {
    await ensureLocalDirectory(this.directory);
    const path = this._path(scopeKey);
    const info = await inspectLocalPath(path, { allowMissing: true });
    if (!info) { this.readDiagnostics.delete(scopeKey); return []; }
    if (info.size > MAX_EXPERIENCE_FILE_BYTES) throw retrievalFailure('Experience cache is too large. / 经验缓存过大。');
    const raw = await readFile(path, 'utf8');
    if (Buffer.byteLength(raw) > MAX_EXPERIENCE_FILE_BYTES) throw retrievalFailure('Experience cache is too large. / 经验缓存过大。');
    const document = JSON.parse(raw);
    if (document.version !== 1 || document.scopeKey !== scopeKey || !Array.isArray(document.entries))
      throw retrievalFailure('Invalid experience version. / 经验版本无效。');
    const now = this.clock(), entries = []; let invalid = 0, expired = 0;
    for (const entry of document.entries.slice(-MAX_EXPERIENCE_ENTRIES)) {
      const createdAt = Date.parse(entry?.createdAt), expiresAt = entry?.expiresAt === undefined
        ? createdAt + MAX_EXPERIENCE_AGE_MS : Date.parse(entry.expiresAt);
      if (!entry || !/^[a-f0-9]{64}$/u.test(entry.id ?? '') || !validText(entry.query, 2000) ||
          !validText(entry.conclusionId, 128) || !validText(entry.state, 128) || !validSourceRefs(entry.sourceRefs) ||
          !validNavigation(entry.navigation ?? []) || !Number.isFinite(createdAt) || !Number.isFinite(expiresAt) ||
          expiresAt <= createdAt || createdAt > now + 60000) { invalid++; continue; }
      if (expiresAt <= now) { expired++; continue; }
      // Old or incomplete records remain navigation hints, never durable verified conclusions.
      // 旧版本或不完整记录仍只能当导航线索，不能升级为持久认证结论。
      entries.push({ ...entry, expiresAt: new Date(expiresAt).toISOString(), correctnessCertified: false,
        recordState: entry.complete !== true || !entry.sourceRefs.length || entry.state !== 'ready-to-answer' ? 'partial' : 'navigation-only' });
    }
    this.readDiagnostics.set(scopeKey, { invalid, expired, omitted: Math.max(0, document.entries.length - MAX_EXPERIENCE_ENTRIES) });
    while (this.readDiagnostics.size > 32) this.readDiagnostics.delete(this.readDiagnostics.keys().next().value);
    return entries;
  }
  save({ scopeKeys, query, sourceRefs, conclusionId, state, navigation = [], complete = true }) {
    return this._run(async () => {
      const scopes = retrievalScopeKeys(scopeKeys);
      if (!validText(query, 2000) || !validSourceRefs(sourceRefs) || !validText(conclusionId, 128) ||
          !validText(state, 128) || !validNavigation(navigation) || typeof complete !== 'boolean')
        throw retrievalFailure('Invalid experience receipt. / 检索经验回执无效。');
      // Partition citations by their authorized scope; no conversation history is shared across chats.
      // 按已授权范围分区引用，不跨聊天共享对话原文；复用时必须重新检查全部来源。
      for (const scopeKey of scopes.filter(scope => scope.startsWith('chat:'))) {
        const entries = await this._read(scopeKey);
        const id = createHash('sha256').update(JSON.stringify([query, sourceRefs, conclusionId])).digest('hex');
        const createdAt = this.clock();
        const entry = { id, query, sourceRefs, conclusionId, state, navigation, complete,
          correctnessCertified: false, createdAt: new Date(createdAt).toISOString(),
          expiresAt: new Date(createdAt + MAX_EXPERIENCE_AGE_MS).toISOString() };
        if (Buffer.byteLength(JSON.stringify(entry)) > 1024 * 1024)
          throw retrievalFailure('Experience receipt is too large. / 检索经验回执过大。');
        let next = [...entries.filter(item => item.id !== id).slice(-63), entry];
        while (Buffer.byteLength(JSON.stringify(next)) > 1024 * 1024) next.shift();
        await atomicJson(this._path(scopeKey), { version: 1, scopeKey, entries: next });
      }
    });
  }
  find({ scopeKeys, query = '', limit = 6 }) {
    return this._run(async () => {
      const entries = (await Promise.all(retrievalScopeKeys(scopeKeys).filter(scope => scope.startsWith('chat:'))
        .map(scope => this._read(scope)))).flat();
      const terms = String(query).normalize('NFKC').toLowerCase().match(/[\p{L}\p{N}_]+/gu) ?? [];
      return [...new Map(entries.filter(entry => !terms.length || terms.some(term => entry.query.toLowerCase().includes(term)))
        .map(entry => [entry.id, entry])).values()].slice(-Math.max(1, Math.min(20, limit))).reverse();
    });
  }
}
