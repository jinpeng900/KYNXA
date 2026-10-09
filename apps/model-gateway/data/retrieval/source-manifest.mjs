import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import { isAbsolute, join, resolve } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';
import { validateSourceExtraction, validateSourceFileWindow } from './retrieval-contracts.mjs';

const SCHEMA_VERSION = 1;
const MAX_MANIFEST_BYTES = 128 * 1024 * 1024;
const MAX_MANIFEST_FILES = 100000;
const META_FIELDS = ['sizeBytes', 'mtimeMs', 'ctimeMs', 'device', 'inode'];

function validateWindows(file) {
  if (file.windows === undefined) return undefined;
  if (!Array.isArray(file.windows) || !file.windows.length || file.windows.length > 4096)
    throw toolFailure('索引窗口清单损坏，原记录已保留。', 'INVALID_RETRIEVAL_MANIFEST', 409);
  const windows = file.windows.map(window => validateSourceFileWindow(window));
  let offset = 0, byte = windows[0].startByte, textBytes = 0, line = 1;
  if (!(['utf-8'].includes(windows[0].encoding) ? [0, 3] : [2]).includes(byte))
    throw toolFailure('索引窗口起点无效。', 'INVALID_RETRIEVAL_MANIFEST', 409);
  for (const window of windows) {
    // Window descriptors cover exactly one version of the original file, without gaps or reordered offsets.
    // 窗口描述必须连续覆盖同一原文件版本，不能复用有缺口、乱序或不同版本的偏移。
    if (window.startOffset !== offset || window.startByte !== byte || window.startLine !== line ||
        window.textContentHash !== file.contentHash || window.encoding !== windows[0].encoding ||
        window.rawContentHash !== windows[0].rawContentHash || window.totalCharacters !== windows[0].totalCharacters ||
        META_FIELDS.some(field => window.metadata[field] !== file.metadata[field]))
      throw toolFailure('索引窗口版本或偏移无效。', 'INVALID_RETRIEVAL_MANIFEST', 409);
    offset = window.endOffset; byte = window.endByte; line = window.endLine; textBytes += window.textBytes;
  }
  if (offset !== windows[0].totalCharacters || byte !== file.metadata.sizeBytes || textBytes !== file.textBytes)
    throw toolFailure('索引窗口未完整覆盖原文件。', 'INVALID_RETRIEVAL_MANIFEST', 409);
  return windows;
}

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
        !Number.isSafeInteger(file.textBytes) || file.textBytes < 0 || file.textBytes > 1024 * 1024 * 1024 ||
        !file.metadata || META_FIELDS.some(field => !Number.isFinite(file.metadata[field])) ||
        ['sizeBytes', 'device', 'inode'].some(field => file.metadata[field] < 0))
      throw toolFailure('索引文件清单损坏，原记录已保留。', 'INVALID_RETRIEVAL_MANIFEST', 409);
    names.add(file.relativePath);
    const windows = validateWindows(file);
    return { relativePath: file.relativePath, contentHash: file.contentHash, textBytes: file.textBytes,
      ...(file.extraction === undefined ? {} : { extraction: validateSourceExtraction(file.extraction) }),
      ...(windows === undefined ? {} : { windows }),
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
      // New extraction metadata must not produce a manifest that the same bounded reader cannot reopen.
      // 新增提取元信息不能写出本读取器下次无法打开的清单；超限保留此前有效文件。
      if (Buffer.byteLength(`${JSON.stringify(document, null, 2)}\n`) > MAX_MANIFEST_BYTES)
        throw toolFailure('索引清单超过存储预算，原清单已保留。', 'INVALID_RETRIEVAL_MANIFEST', 413);
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
