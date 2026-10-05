import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const MODEL_REVISION = '761b726dd34fb83930e26aab4e9ac3899aa1fa78';
const MODEL_SOURCE = `https://huggingface.co/Xenova/multilingual-e5-small/resolve/${MODEL_REVISION}`;

// The checked-in manifest pins converted weights, tokenization, and attribution.
// 仓库清单固定转换权重、分词资源和归属信息；大文件在构建时校验还原并随安装包提供。
export const BUILTIN_EMBEDDING_PROFILE = Object.freeze({
  id: 'builtin-multilingual',
  modelId: 'Xenova/multilingual-e5-small',
  modelVersion: `${MODEL_REVISION}:q8`,
  revision: MODEL_REVISION,
  dimensions: 384,
  maxInputTokens: 512,
  queryPrefix: 'query: ',
  documentPrefix: 'passage: ',
  dtype: 'q8',
  license: 'MIT',
  files: Object.freeze([
    { path: 'config.json', url: `${MODEL_SOURCE}/config.json`, bytes: 658, sha256: 'cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1' },
    { path: 'tokenizer_config.json', url: `${MODEL_SOURCE}/tokenizer_config.json`, bytes: 443, sha256: 'a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b' },
    { path: 'special_tokens_map.json', url: `${MODEL_SOURCE}/special_tokens_map.json`, bytes: 167, sha256: 'd05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7' },
    { path: 'tokenizer.json', url: `${MODEL_SOURCE}/tokenizer.json`, bytes: 17082730, sha256: '0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39' },
    { path: 'onnx/model_quantized.onnx', url: `${MODEL_SOURCE}/onnx/model_quantized.onnx`, bytes: 118308185, sha256: 'f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193' },
    { path: 'MODEL_CARD.md', url: 'https://huggingface.co/intfloat/multilingual-e5-small/raw/614241f622f53c4eeff9890bdc4f31cfecc418b3/README.md', bytes: 497538, sha256: '0038de97aee16258cecbad7ffda4b4febd6953e747a00e0ddbc8e6ed241e9c1c' },
    { path: 'CONVERSION_MODEL_CARD.md', url: `https://huggingface.co/Xenova/multilingual-e5-small/raw/${MODEL_REVISION}/README.md`, bytes: 1077, sha256: '561a19594636657fe033f8b4427a7743b5f6f3a12f16cecc5f286feca0453245' },
    { path: 'LICENSE', url: 'https://raw.githubusercontent.com/microsoft/unilm/0e31c7c09737df491e7ff74ded19614b884c52b4/LICENSE', bytes: 1104, sha256: '904dc4d8749877f1dba1cda48200d2462dccbeb7c134d5e4ef6fa75e0198c8fe' },
    { path: 'licenses/onnxruntime-MIT.txt', url: 'https://raw.githubusercontent.com/microsoft/onnxruntime/v1.21.0/LICENSE', bytes: 1073, sha256: '2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c' },
    { path: 'licenses/onnxruntime-ThirdPartyNotices.txt', url: 'https://raw.githubusercontent.com/microsoft/onnxruntime/v1.21.0/ThirdPartyNotices.txt', bytes: 316114, sha256: '8c06e8cff286a4a117b3b246a4c7da68428a144af757823db50e3d6520941ec6' },
    { path: 'licenses/sqlite-vec-MIT.txt', url: 'https://raw.githubusercontent.com/asg017/sqlite-vec/v0.1.9/LICENSE-MIT', bytes: 1068, sha256: '6ce72bbe12d975bd5286e5ab0a064c069693300c47bccbc57bec18485f1621ea' },
  ].map(Object.freeze)),
});

export function embeddingBuildCacheRoot() {
  const repositoryRoot = fileURLToPath(new URL('../../../../', import.meta.url));
  return join(repositoryRoot, 'artifacts', 'runtime', 'embedding', BUILTIN_EMBEDDING_PROFILE.id);
}

export function defaultEmbeddingModelRoot() {
  // Runtime assets are siblings of the installed gateway, not source modules.
  // 安装包运行资产与网关目录并列，不把尚未构建的 runtime 目录当作源码导入。
  const applicationRoot = fileURLToPath(new URL('../../../', import.meta.url));
  const installedRoot = join(applicationRoot, 'runtime', 'embedding', BUILTIN_EMBEDDING_PROFILE.id);
  return existsSync(installedRoot) ? installedRoot : embeddingBuildCacheRoot();
}
