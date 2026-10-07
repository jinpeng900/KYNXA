import { hashText, retrievalFailure, validateChunkStructure, validateStructureDescriptor } from './retrieval-contracts.mjs';
import { CHUNKER_VERSION, TOKENIZER_VERSION, EMBEDDING_TEXT_VERSION,
  embeddingTextForChunk, lexicalText } from './retrieval-text.mjs';

export const DEFAULT_PARSER_VERSION = 'plain-text-v1';
export const DERIVATION_VERSION = 'source-derivation-v1';

function versionTag(value, fallback, label) {
  value ??= fallback;
  if (typeof value !== 'string' || !value.trim() || value.length > 512 || /[\x00-\x1f]/u.test(value))
    throw retrievalFailure(`Invalid ${label} version. / 检索派生版本无效。`, 'INVALID_RETRIEVAL_DERIVATION');
  return value;
}

/** Canonical metadata prevents object property ordering from changing a durable derivation.
 * 元信息按键排序，避免对象属性顺序改变持久派生身份。 */
function canonicalMetadata(value) {
  if (Array.isArray(value)) return value.map(canonicalMetadata);
  if (value && typeof value === 'object') return Object.fromEntries(Object.keys(value).sort()
    .map(key => [key, canonicalMetadata(value[key])]));
  return value;
}

export function sourceDerivationVersions(source) {
  return {
    parserVersion: versionTag(source.parserVersion, source.structure?.parserVersion ?? DEFAULT_PARSER_VERSION, 'parser'),
    chunkerVersion: versionTag(source.chunkerVersion, CHUNKER_VERSION, 'chunker'),
    tokenizerVersion: versionTag(source.tokenizerVersion, TOKENIZER_VERSION, 'tokenizer'),
    embeddingInputVersion: versionTag(source.embeddingInputVersion, EMBEDDING_TEXT_VERSION, 'embedding input')
  };
}

/** Separate lexical derivation from actual embedding input, so safe lexical refreshes retain vectors.
 * 区分词法派生与实际嵌入输入，安全的词法刷新可以保留向量，输入改变则必须重新嵌入。 */
export function deriveSourceVersion(source, chunks, { checkCancelled = () => {}, embeddingMaxCharacters = 512 } = {}) {
  const versions = sourceDerivationVersions(source);
  const structure = validateStructureDescriptor(source.structure);
  if (structure && structure.parserVersion !== versions.parserVersion)
    throw retrievalFailure('Source parser versions differ. / 资料解析版本不一致。', 'INVALID_RETRIEVAL_STRUCTURE');
  const derivationChunks = [], embeddingChunks = [];
  for (const chunk of chunks) {
    checkCancelled();
    const topology = { chunkId: chunk.chunkId, chunkIndex: chunk.chunkIndex, chunkHash: chunk.chunkHash,
      startOffset: chunk.startOffset, endOffset: chunk.endOffset, startLine: chunk.startLine, endLine: chunk.endLine,
      ...(chunk.structure !== undefined ? { structure: validateChunkStructure(chunk.structure, source, chunk) } : {}) };
    derivationChunks.push({ ...topology,
      chunkerVersion: versionTag(chunk.chunkerVersion, versions.chunkerVersion, 'chunker'),
      tokenizerVersion: versionTag(chunk.tokenizerVersion, versions.tokenizerVersion, 'tokenizer'),
      lexicalHash: hashText(lexicalText(`${source.title} ${source.locator.relativePath ?? ''} ${chunk.text}`)) });
    embeddingChunks.push({ ...topology, chunkerVersion: versionTag(chunk.chunkerVersion, versions.chunkerVersion, 'chunker'),
      inputHash: hashText(embeddingTextForChunk(source, chunk, { maxChars: embeddingMaxCharacters })) });
  }
  checkCancelled();
  const metadata = { sourceId: source.sourceId, scopeKey: source.scopeKey, contentHash: source.contentHash,
    sourceRevision: source.sourceRevision, bindingRevision: source.bindingRevision ?? 0,
    sourceType: source.sourceType, title: source.title, locator: canonicalMetadata(source.locator),
    ...(structure ? { structure } : {}) };
  const embeddingInputSignature = hashText(JSON.stringify({ version: DERIVATION_VERSION,
    parserVersion: versions.parserVersion, chunkerVersion: versions.chunkerVersion, embeddingInputVersion: versions.embeddingInputVersion,
    ...(structure ? { structure } : {}), chunks: embeddingChunks }));
  return { ...versions,
    derivationSignature: hashText(JSON.stringify({ version: DERIVATION_VERSION, ...versions,
      source: metadata, chunks: derivationChunks, embeddingInputSignature })),
    embeddingInputSignature };
}
