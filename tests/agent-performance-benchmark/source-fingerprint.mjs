import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

const SOURCE_FILES = Object.freeze([
  'apps/model-gateway/orchestration/runtime.mjs',
  'apps/model-gateway/orchestration/retrieval/source-projection.mjs',
  'apps/model-gateway/orchestration/retrieval/coordinator.mjs',
  'apps/model-gateway/orchestration/retrieval/candidate-selection.mjs',
  'apps/model-gateway/orchestration/retrieval/evidence-acquisition.mjs',
  'apps/model-gateway/orchestration/tool-loop.mjs',
  'apps/model-gateway/data/retrieval/evidence-references.mjs',
  'apps/model-gateway/data/retrieval/source-window.mjs',
  'apps/model-gateway/data/retrieval/index.mjs',
  'apps/model-gateway/data/tool-result-store.mjs',
  'apps/model-gateway/data/retrieval/index-worker.mjs',
  'apps/model-gateway/data/retrieval/retrieval-text.mjs',
  'apps/model-gateway/models/model-history.mjs',
  'apps/model-gateway/models/tool-context.mjs',
  'apps/model-gateway/models/tool-observation-compaction.mjs',
  'apps/model-gateway/tools/tool-service.mjs',
  'apps/model-gateway/tools/retrieval/descriptors.mjs',
  'apps/model-gateway/models/protocols.mjs',
  'tests/agent-performance-benchmark/run-agent.mjs',
  'tests/agent-performance-benchmark/evaluation.mjs',
  'tests/agent-performance-benchmark/evaluation.test.mjs',
  'tests/agent-performance-benchmark/rescore.mjs',
  'tests/agent-performance-benchmark/source-fingerprint.mjs',
  'tests/agent-performance-benchmark/harness.test.mjs'
]);
const sha256 = value => createHash('sha256').update(value).digest('hex');

/**
 * Hash only the explicit repository source allowlist; never scan configured Data, connections or assets.
 * 仅哈希明确列出的仓库源码，不扫描配置 Data、模型连接或其他资产，不为旧运行补造源码指纹。
 */
export async function readSourceFingerprint() {
  const checked = await Promise.allSettled(SOURCE_FILES.map(async path => ({ path,
    sha256: sha256(await readFile(fileURLToPath(new URL(`../../${path}`, import.meta.url)))) })));
  if (checked.some(item => item.status !== 'fulfilled'))
    throw Object.assign(new Error('BENCHMARK_SOURCE_FINGERPRINT_FAILED'), { code: 'BENCHMARK_SOURCE_FINGERPRINT_FAILED' });
  const files = checked.map(item => item.value);
  return { version: 1, algorithm: 'sha256', files, combinedHash: sha256(JSON.stringify(files)) };
}
