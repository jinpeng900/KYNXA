import { randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, writeFile, unlink } from 'node:fs/promises';
import { dirname } from 'node:path';

// Shared persistence primitives must not depend on model configuration or repositories.
// 共用持久化基础不依赖模型配置或具体仓储，调用方继续拥有数据语义。
export async function readJson(filename, fallback) {
  try { return JSON.parse(await readFile(filename, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return fallback; throw error; }
}

export async function atomicJson(filename, value, { signal } = {}) {
  signal?.throwIfAborted();
  await mkdir(dirname(filename), { recursive: true, mode: 0o700 });
  signal?.throwIfAborted();
  const temp = `${filename}.${randomUUID()}.tmp`;
  try {
    await writeFile(temp, JSON.stringify(value, null, 2), { mode: 0o600, flag: 'wx', signal });
    signal?.throwIfAborted();
    // A dispatched atomic rename must return its actual receipt, even if cancellation arrives.
    // 原子重命名发起后等待实际回执，随后取消不能将已经提交的文件报告成未执行。
    await rename(temp, filename);
  } finally { await unlink(temp).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
}
