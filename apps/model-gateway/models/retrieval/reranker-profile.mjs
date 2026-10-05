import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const MODEL_REVISION = '280bcc27a84e0b898c251e06fddb25171bd9b101';
const MODEL_SOURCE = `https://huggingface.co/Xenova/bge-reranker-base/resolve/${MODEL_REVISION}`;

// A fixed local cross-encoder scores query/passage pairs, independently from embedding retrieval.
// 固定的本地交叉编码器评价问题与片段配对；它独立于嵌入检索，不将相关性分数当答案置信度。
export const BUILTIN_RERANKER_PROFILE = Object.freeze({
  id: 'builtin-multilingual-reranker', modelId: 'Xenova/bge-reranker-base',
  revision: MODEL_REVISION, modelVersion: `${MODEL_REVISION}:q8`, dtype: 'q8',
  maxInputTokens: 512, maxCandidates: 32, license: 'MIT',
  files: Object.freeze([
    { path: 'config.json', url: `${MODEL_SOURCE}/config.json`, bytes: 782, sha256: 'b6575b9d5be20d6747417c8e20c5a0db1636356e0b6d422d7244c628423c4d4c' },
    { path: 'tokenizer_config.json', url: `${MODEL_SOURCE}/tokenizer_config.json`, bytes: 443, sha256: 'a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b' },
    { path: 'special_tokens_map.json', url: `${MODEL_SOURCE}/special_tokens_map.json`, bytes: 279, sha256: 'd5469a60db23249c7f8945013d78df30b44b6bf686c6bb4740f4223f77b1b535' },
    { path: 'tokenizer.json', url: `${MODEL_SOURCE}/tokenizer.json`, bytes: 17098079, sha256: '48564c5c7d3fa64d85d95e65414a542385f88b0f128fd8d4163fd7a57f2be05c' },
    { path: 'onnx/model_quantized.onnx', url: `${MODEL_SOURCE}/onnx/model_quantized.onnx`, bytes: 279301077, sha256: 'dd98f3e67837d23210a6b7550c08cced4f61845b940ac45be3565840a10f3244' },
    { path: 'MODEL_CARD.md', url: 'https://huggingface.co/BAAI/bge-reranker-base/raw/2cfc18c9415c912f9d8155881c133215df768a70/README.md', bytes: 34111, sha256: 'b4223c8c1e95538f8c79168afc501f8b2da06c63221f013b79b28af78b59366d' },
    { path: 'CONVERSION_MODEL_CARD.md', url: `${MODEL_SOURCE}/README.md`, bytes: 1135, sha256: '2ccb3f72b5eb6205316c0b9dde944d6f944ec4a87b85706ce07ef9d9d02b4690' },
    { path: 'LICENSE', url: 'https://raw.githubusercontent.com/FlagOpen/FlagEmbedding/c086741f5e117b7b8ce1745ea00b6c262f281a01/LICENSE', bytes: 1065, sha256: '587a673933425dbc36ec61268d3b954051b2d3ef3c9b322ede357976055ffdd5' },
  ].map(Object.freeze)),
});

export function rerankerBuildCacheRoot() {
  return join(fileURLToPath(new URL('../../../../', import.meta.url)), 'artifacts', 'runtime', 'rerank', BUILTIN_RERANKER_PROFILE.id);
}

export function defaultRerankerModelRoot() {
  const installed = join(fileURLToPath(new URL('../../../', import.meta.url)), 'runtime', 'rerank', BUILTIN_RERANKER_PROFILE.id);
  return existsSync(installed) ? installed : rerankerBuildCacheRoot();
}
