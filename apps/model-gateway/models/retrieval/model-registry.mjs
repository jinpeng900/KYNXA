import { createHash } from 'node:crypto';
import { BUILTIN_EMBEDDING_PROFILE } from './embedding-profile.mjs';
import { BUILTIN_RERANKER_PROFILE } from './reranker-profile.mjs';

const RUNTIME_ASSET_PATHS = new Set(['config.json', 'tokenizer_config.json', 'special_tokens_map.json',
  'tokenizer.json', 'onnx/model_quantized.onnx']);

function signature(value) {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex');
}

function registerProfile(kind, profile, implementationId, inputProjection) {
  const assetManifest = profile.files.map(({ path, bytes, sha256 }) => ({ path, bytes, sha256 }));
  const modelAssetSignature = signature(assetManifest.filter(asset => RUNTIME_ASSET_PATHS.has(asset.path)));
  const descriptor = { ...profile, kind, implementationId, local: true, network: false,
    languages: Object.freeze(['zh', 'en', 'multilingual']), tokenizerId: 'xlm-roberta',
    inputProjection: Object.freeze(inputProjection), inputProjectionVersion: inputProjection.version,
    modelAssetSignature, assetSignature: signature(assetManifest) };
  if (kind === 'embedding') {
    // The vector space follows model and projection identity, not a display name or data chunk version.
    // 向量空间绑定模型及投影身份，不由展示名称或数据分块版本决定。
    descriptor.embeddingSpaceId = signature({ implementationId, modelId: profile.modelId,
      modelVersion: profile.modelVersion, dimensions: profile.dimensions, inputProjection, modelAssetSignature });
  }
  return Object.freeze(descriptor);
}

const EMBEDDING = registerProfile('embedding', BUILTIN_EMBEDDING_PROFILE, 'transformers-onnx-feature-extraction-v1', {
  version: 'e5-prefixed-mean-l2-v1', queryPrefix: BUILTIN_EMBEDDING_PROFILE.queryPrefix,
  documentPrefix: BUILTIN_EMBEDDING_PROFILE.documentPrefix, pooling: 'mean', normalize: true,
});
const RERANKER = registerProfile('reranker', BUILTIN_RERANKER_PROFILE, 'transformers-onnx-cross-encoder-v1', {
  version: 'query-passage-pair-sigmoid-v1', pairing: 'text_pair', scoring: 'sigmoid',
});
const PROFILES = Object.freeze([EMBEDDING, RERANKER]);

export class RetrievalModelProfileError extends Error {
  constructor(kind, profileId) {
    super(`Unsupported local ${kind} profile. / 本地检索模型配置未实现。`);
    this.name = 'RetrievalModelProfileError';
    this.code = 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED';
    this.details = { kind, profileId: typeof profileId === 'string' ? profileId : null };
  }
}

export function listRetrievalModelProfiles(kind) {
  return PROFILES.filter(profile => kind === undefined || profile.kind === kind);
}

export function resolveRetrievalModelProfile(kind, profileId) {
  const profile = PROFILES.find(item => item.kind === kind && item.id === profileId);
  if (!profile) throw new RetrievalModelProfileError(kind, profileId);
  return profile;
}

export function retrievalModelMetadata(profile) {
  return { profileId: profile.id, modelId: profile.modelId, modelVersion: profile.modelVersion,
    implementationId: profile.implementationId, inputProjectionVersion: profile.inputProjectionVersion,
    modelAssetSignature: profile.modelAssetSignature, assetSignature: profile.assetSignature,
    ...(profile.kind === 'embedding' ? { dimensions: profile.dimensions, embeddingSpaceId: profile.embeddingSpaceId } : {}) };
}

export function unavailableProfileStatus(kind, profileId) {
  return { profileId: typeof profileId === 'string' ? profileId : null, kind, state: 'unavailable',
    loaded: false, supported: false, local: true, network: false,
    errorCode: 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED' };
}
