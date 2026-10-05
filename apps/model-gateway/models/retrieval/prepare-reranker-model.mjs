import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { prepareLocalModel } from './prepare-embedding-model.mjs';
import { BUILTIN_RERANKER_PROFILE, rerankerBuildCacheRoot } from './reranker-profile.mjs';

// Reuse the same integrity checks, exclusive build lock and explicit packaging whitelist.
// 复用同一套完整性校验、独占构建锁与明确的打包白名单，不在运行时下载权重。
export function prepareRerankerModel({ modelRoot = rerankerBuildCacheRoot(), offline = false } = {}) {
  return prepareLocalModel({ profile: BUILTIN_RERANKER_PROFILE, modelRoot, offline });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const rootIndex = process.argv.indexOf('--model-root');
  const modelRoot = rootIndex >= 0 ? process.argv[rootIndex + 1] : undefined;
  if (rootIndex >= 0 && !modelRoot) throw new Error('--model-root requires a directory.');
  const root = await prepareRerankerModel({ modelRoot, offline: process.argv.includes('--offline') });
  console.log(`Verified bundled reranking assets: ${root}`);
}
