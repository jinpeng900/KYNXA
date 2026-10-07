import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';

const SCHEMA_VERSION = 1;
const MAX_MANIFEST_BYTES = 128 * 1024 * 1024;
const MAX_MANIFEST_FILES = 100000;
const META_FIELDS = ['sizeBytes', 'mtimeMs', 'ctimeMs', 'device', 'inode'];

function validateBinding(binding) {
  if (!binding || typeof binding.projectId !== 'string' || !binding.projectId || binding.projectId.length > 256 ||
      typeof binding.root !== 'string' || !isAbsolute(binding.root) ||
      !Number.isSafeInteger(binding.bindingRevision) || binding.bindingRevision < 0)
    throw toolFailure('索引文件清单绑定无效。', 'INVALID_RETRIEVAL_MANIFEST', 400);
  return { projectId: binding.projectId.toLowerCase(), root: resolve(binding.root), bindingRevision: binding.bindingRevision };
}

function validateFiles(files) {
  if (!Array.isArray(files) || files.length > MAX_MANIFEST_FILES)
    throw toolFailure('索引文件清单超过允许范围。', 'INVALID_RETRIEVAL_MANIFEST', 400);
  const names = new Set();
  return files.map(file => {
    if (!file || typeof file.relativePath !== 'string' || !file.relativePath || file.relativePath.length > 4096 ||
        isAbsolute(file.relativePath) || /[\x00-\x1f]/u.test(file.relativePath) ||
        file.relativePath.split(/[\\/]/u).some(part => ['.', '..', ''].includes(part)) || names.has(file.relativePath) ||
        typeof file.contentHash !== 'string' || !/^[a-f0-9]{64}$/u.test(file.contentHash) ||
        !Number.isSafeInteger(file.textBytes) || file.textBytes < 0 || file.textBytes > 8 * 1024 * 1024 ||
        !file.metadata || META_FIELDS.some(field => !Number.isFinite(file.metadata[field])) ||
        ['sizeBytes', 'device', 'inode'].some(field => file.metadata[field] < 0))
      throw toolFailure('索引文件清单损坏，原记录已保留。', 'INVALID_RETRIEVAL_MANIFEST', 409);
    names.add(file.relativePath);
    return { relativePath: file.relativePath, contentHash: file.contentHash, textBytes: file.textBytes,
      metadata: Object.fromEntries(META_FIELDS.map(field => [field, file.metadata[field]])) };
  });
}

/** Rebuildable file metadata only: a scanned hash is never proof that parsing or indexing committed.
 * 只保存可重建的文件元信息，扫描得到的哈希不能证明解析或索引已经提交。 */
export class SourceManifestStore {
  constructor(root) { this.folder = join(root, 'Retrieval', 'SourceManifests'); this.queue = Promise.resolve(); }

  pathFor(binding) {
    const identity = createHash('sha256').update(JSON.stringify(validateBinding(binding))).digest('hex');
    return join(this.folder, `${identity}.json`);
  }

  async read(binding, { signal } = {}) {
    signal?.throwIfAborted();
    const expected = validateBinding(binding), path = this.pathFor(expected);
    const info = await inspectLocalPath(path, { allowMissing: true });
    if (!info) return null;
    if (!info.isFile() || info.size > MAX_MANIFEST_BYTES)
      throw toolFailure('索引文件清单过大或路径无效。', 'INVALID_RETRIEVAL_MANIFEST', 409);
    const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
    let value;
    try {
      const opened = await handle.stat();
      if (!opened.isFile() || opened.nlink !== 1 || opened.dev !== info.dev || opened.ino !== info.ino)
        throw toolFailure('索引清单路径已经变化。', 'UNSAFE_TOOL_PATH', 403);
      value = JSON.parse(await handle.readFile({ encoding: 'utf8', signal }));
      const after = await inspectLocalPath(path);
      if (after.dev !== opened.dev || after.ino !== opened.ino || after.size !== opened.size || after.mtimeMs !== opened.mtimeMs)
        throw toolFailure('索引清单读取期间变化。', 'STALE_RETRIEVAL_SOURCE', 409);
    } finally { await handle.close(); }
    signal?.throwIfAborted();
    if (value.schemaVersion !== SCHEMA_VERSION || JSON.stringify(validateBinding(value.binding)) !== JSON.stringify(expected))
      throw toolFailure('索引文件清单版本或绑定无效。', 'INVALID_RETRIEVAL_MANIFEST', 409);
    return { schemaVersion: SCHEMA_VERSION, binding: expected, files: validateFiles(value.files) };
  }

  write(binding, files, { signal } = {}) {
    const pending = this.queue.catch(() => {}).then(async () => {
      signal?.throwIfAborted();
      const document = { schemaVersion: SCHEMA_VERSION, binding: validateBinding(binding), files: validateFiles(files),
        scannedAt: new Date().toISOString() };
      await ensureLocalDirectory(this.folder);
      const path = this.pathFor(document.binding);
      await inspectLocalPath(path, { allowMissing: true });
      await atomicJson(path, document, { signal });
      return { committed: true, fileCount: document.files.length };
    });
    this.queue = pending;
    return pending;
  }

  async drain() { await this.queue.catch(() => {}); }
}
