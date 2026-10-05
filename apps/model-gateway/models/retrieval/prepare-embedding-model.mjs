import { createHash, randomUUID } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { mkdir, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { dirname, join, resolve } from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { fileURLToPath } from 'node:url';
import { BUILTIN_EMBEDDING_PROFILE, embeddingBuildCacheRoot } from './embedding-profile.mjs';
import { verifyEmbeddingAsset } from './embedding-assets.mjs';

async function lockModelPreparation(bundleRoot) {
  const canonicalRoot = await realpath(bundleRoot);
  const lockKey = createHash('sha256').update(process.platform === 'win32' ? canonicalRoot.toLowerCase() : canonicalRoot).digest('hex');
  const address = process.platform === 'win32' ? { path: `\\\\.\\pipe\\kynxa-embedding-build-${lockKey}` }
    : { host: '127.0.0.1', port: 20000 + Number.parseInt(lockKey.slice(0, 8), 16) % 40000 };
  const deadlineMs = Date.now() + 180_000;
  for (;;) {
    const lockServer = createServer(socket => socket.destroy());
    try {
      await new Promise((resolveLock, rejectLock) => {
        lockServer.once('error', rejectLock);
        lockServer.listen({ ...address, exclusive: true }, resolveLock);
      });
      return () => new Promise((resolveClose, rejectClose) => lockServer.close(error => error ? rejectClose(error) : resolveClose()));
    } catch (error) {
      if (error.code !== 'EADDRINUSE') throw error;
      if (Date.now() >= deadlineMs) throw new Error('Timed out waiting for local model asset preparation.');
      await new Promise(resolveWait => setTimeout(resolveWait, 200));
    }
  }
}

// Build tooling downloads only pinned public assets; inference never downloads.
// 仅构建工具下载固定版本的公开资产；应用推理阶段不联网下载，也不上传用户文本。
export async function prepareLocalModel({ profile, modelRoot, offline = false }) {
  const bundleRoot = resolve(modelRoot);
  const assetLabel = profile === BUILTIN_EMBEDDING_PROFILE ? 'embedding' : 'local model';
  await mkdir(bundleRoot, { recursive: true });
  // OS-owned exclusive endpoints are released even if a build is terminated.
  // 操作系统持有独占端点；构建被强制结束时自动释放，不留下阻断以后构建的锁文件。
  const releaseLock = await lockModelPreparation(bundleRoot);
  try {
    for (const asset of profile.files) {
      if (await verifyEmbeddingAsset(bundleRoot, asset).catch(error => {
        if (error.code === 'ENOENT') return false;
        throw error;
      })) continue;
      if (offline) throw new Error(`Pinned ${assetLabel} asset is unavailable or damaged: ${asset.path}`);
      const destination = join(bundleRoot, asset.path);
      const pendingPath = `${destination}.download-${randomUUID()}`;
      await mkdir(dirname(destination), { recursive: true });
      try {
        const downloadTimeoutMs = Math.min(900_000, Math.max(180_000, Math.ceil(asset.bytes / (256 * 1024)) * 1000 + 30_000));
        const signal = AbortSignal.timeout(downloadTimeoutMs);
        const response = await fetch(asset.url, { signal });
        if (!response.ok || !response.body) throw new Error(`${assetLabel} asset download failed: HTTP ${response.status}`);
        await pipeline(Readable.fromWeb(response.body), createWriteStream(pendingPath, { flags: 'wx' }), { signal });
        if (!await verifyEmbeddingAsset(dirname(pendingPath), { ...asset, path: pendingPath.split(/[\\/]/u).at(-1) })) {
          throw new Error(`Pinned ${assetLabel} asset failed SHA-256 verification: ${asset.path}`);
        }
        await rename(pendingPath, destination);
      } finally {
        await rm(pendingPath, { force: true });
      }
    }
    await writeFile(join(bundleRoot, 'manifest.json'), `${JSON.stringify(profile, null, 2)}\n`, 'utf8');
    // Package only verified public assets, never arbitrary build-cache contents.
    // 安装包仅包含已校验的公开资产，不递归带入构建缓存中的其他文件。
    const bundleFiles = [...profile.files.map(asset => join(bundleRoot, asset.path)), join(bundleRoot, 'manifest.json')];
    await writeFile(join(bundleRoot, 'bundle-files.txt'), `${bundleFiles.join('\n')}\n`, 'utf8');
    return bundleRoot;
  } finally {
    await releaseLock();
  }
}

export function prepareEmbeddingModel({ modelRoot = embeddingBuildCacheRoot(), offline = false } = {}) {
  return prepareLocalModel({ profile: BUILTIN_EMBEDDING_PROFILE, modelRoot, offline });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const cacheIndex = process.argv.indexOf('--model-root');
  const modelRoot = cacheIndex >= 0 ? process.argv[cacheIndex + 1] : undefined;
  if (cacheIndex >= 0 && !modelRoot) throw new Error('--model-root requires a directory.');
  const bundleRoot = await prepareEmbeddingModel({ modelRoot, offline: process.argv.includes('--offline') });
  console.log(`Verified bundled multilingual embedding assets: ${bundleRoot}`);
}
