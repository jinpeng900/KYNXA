import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import { revalidateLocalPathBinding } from '../platform/tool-paths.mjs';

const MAX_TRACKED_TARGETS = 64;
const MAX_HASH_BYTES = 1024 * 1024;
const hash = value => createHash('sha256').update(value).digest('hex');
const stamp = value => value && [value.dev, value.ino, value.size, value.mtimeMs, value.ctimeMs].join(':');

/** Observe only previously authorized filesystem targets, not an unbounded repository scan.
 * 只观察先前已获准派发的文件目标，不扫描全仓；签名仅证明所列目标，不能证明所有测试依赖。 */
export class ToolCodeVersions {
  constructor() { this.targets = new Map(); this.overflow = false; }

  observe(binding) {
    if (!binding) return;
    if (!this.targets.has(binding.path) && this.targets.size >= MAX_TRACKED_TARGETS) { this.overflow = true; return; }
    this.targets.set(binding.path, { ...binding, allowMissing: true });
  }

  async capture({ signal, assertReadable = () => {} } = {}) {
    const files = [];
    for (const binding of this.targets.values()) {
      signal?.throwIfAborted();
      const pathId = hash(binding.path);
      try {
        assertReadable(binding.path);
        const info = await revalidateLocalPathBinding(binding);
        if (!info || info.isDirectory()) { files.push({ pathId, state: info ? 'directory' : 'missing' }); continue; }
        if (!info.isFile() || info.size > MAX_HASH_BYTES) throw Object.assign(new Error('Unbounded target'), { code: 'CODE_VERSION_TARGET_LIMIT' });
        const handle = await open(binding.path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
        try {
          // Confirm the opened object before reading; a post-read identity check alone is too late for a swapped path.
          // 读取前确认已打开对象；仅在读后核验身份，无法及时阻止路径被替换后的错误读取。
          const opened = await handle.stat();
          if (!opened.isFile() || stamp(info) !== stamp(opened))
            throw Object.assign(new Error('Target changed before hashing'), { code: 'CODE_VERSION_CHANGED' });
          const bytes = Buffer.alloc(Math.min(MAX_HASH_BYTES + 1, opened.size + 1));
          let length = 0;
          while (length < bytes.length) {
            signal?.throwIfAborted();
            const result = await handle.read(bytes, length, bytes.length - length, length);
            if (!result.bytesRead) break;
            length += result.bytesRead;
          }
          const after = await revalidateLocalPathBinding(binding);
          if (length !== opened.size || length > MAX_HASH_BYTES || stamp(info) !== stamp(await handle.stat()) || stamp(info) !== stamp(after))
            throw Object.assign(new Error('Target changed during hashing'), { code: 'CODE_VERSION_CHANGED' });
          files.push({ pathId, state: 'present', sha256: hash(bytes.subarray(0, length)) });
        } finally { await handle.close(); }
      } catch (error) {
        signal?.throwIfAborted();
        files.push({ pathId, state: 'unavailable', code: /^[A-Z][A-Z0-9_]{0,79}$/.test(error.code ?? '') ? error.code : 'CODE_VERSION_UNAVAILABLE' });
      }
    }
    files.sort((left, right) => left.pathId.localeCompare(right.pathId));
    return { signature: hash(JSON.stringify(files)), complete: files.some(file => ['present', 'missing'].includes(file.state)) &&
        !this.overflow && files.every(file => file.state !== 'unavailable'),
      files, overflow: this.overflow, coverage: 'observed-filesystem-targets', exhaustive: false };
  }
}
