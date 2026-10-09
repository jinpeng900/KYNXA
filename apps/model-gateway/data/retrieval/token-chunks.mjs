import { hashText, retrievalFailure } from './retrieval-contracts.mjs';
import { lexicalText, CHUNKER_VERSION, embeddingTextForChunk } from './retrieval-text.mjs';

export const TOKEN_FITTED_CHUNKER_VERSION = 'token-fitted-units-v1';

export function validateEmbeddingProjection(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).some(key => !['context', 'tokenCount', 'maxInputTokens', 'version'].includes(key)) ||
      typeof value.context !== 'string' || value.context.length > 256 || /[\x00-\x08\x0b\x0c\x0e-\x1f]/u.test(value.context) ||
      typeof value.version !== 'string' || !value.version || value.version.length > 128 || /[\x00-\x1f]/u.test(value.version) ||
      !Number.isSafeInteger(value.maxInputTokens) || value.maxInputTokens < 1 || value.maxInputTokens > 32768 ||
      !Number.isSafeInteger(value.tokenCount) || value.tokenCount < 1 || value.tokenCount > value.maxInputTokens)
    throw retrievalFailure('Invalid fitted embedding projection. / token 分块投影无效。', 'INVALID_RETRIEVAL_TOKEN_FIT');
  return { context: value.context, tokenCount: value.tokenCount, maxInputTokens: value.maxInputTokens, version: value.version };
}

function splitsCodePoint(text, offset) {
  return offset > 0 && offset < text.length && /[\uD800-\uDBFF]/u.test(text[offset - 1]) && /[\uDC00-\uDFFF]/u.test(text[offset]);
}

/** Tokenizer receipts subdivide real source ranges; metadata never becomes cited source text.
 * 分词器回执只细分真实来源范围，投影元信息不进入引用原文。 */
export function applyTokenFit(source, chunks, projections, receipt, { checkCancelled = () => {} } = {}) {
  if (!Array.isArray(receipt?.documents) || receipt.documents.length !== chunks.length || projections.length !== chunks.length ||
      typeof receipt.fittingVersion !== 'string' || !Number.isSafeInteger(receipt.maxInputTokens))
    throw retrievalFailure('Token fitting receipt differs from its source batch. / token 分块回执与来源批次不匹配。', 'INVALID_RETRIEVAL_TOKEN_FIT');
  const fitted = [], sourcePrefix = hashText(source.sourceId).slice(0, 24);
  for (const [index, chunk] of chunks.entries()) {
    checkCancelled();
    const document = receipt.documents[index], projection = projections[index];
    if (projection.text !== chunk.text || !Array.isArray(document?.segments) || !document.segments.length)
      throw retrievalFailure('Token fitting body or ranges are missing. / token 分块正文或范围缺失。', 'INVALID_RETRIEVAL_TOKEN_FIT');
    let previousEnd = 0, startLine = chunk.startLine;
    for (const segment of document.segments) {
      checkCancelled();
      if (!Number.isSafeInteger(segment.start) || !Number.isSafeInteger(segment.end) || segment.start !== previousEnd ||
          segment.end <= segment.start || segment.end > chunk.text.length ||
          splitsCodePoint(chunk.text, segment.start) || splitsCodePoint(chunk.text, segment.end))
        throw retrievalFailure('Token fitting must cover original text exactly. / token 分块必须完整覆盖原文且保持 Unicode 边界。', 'INVALID_RETRIEVAL_TOKEN_FIT');
      const text = chunk.text.slice(segment.start, segment.end), chunkHash = hashText(text), chunkIndex = fitted.length;
      const embeddingProjection = validateEmbeddingProjection({ context: projection.context,
        tokenCount: segment.tokenCount, maxInputTokens: receipt.maxInputTokens, version: receipt.fittingVersion });
      const endLine = startLine + (text.match(/\n/gu)?.length ?? 0);
      fitted.push({ ...chunk, chunkIndex, chunkId: `${sourcePrefix}:${chunkHash.slice(0, 24)}:${chunkIndex}`,
        text, chunkHash, startOffset: chunk.startOffset + segment.start, endOffset: chunk.startOffset + segment.end,
        startLine, endLine, lexicalText: lexicalText(`${source.title} ${source.locator?.relativePath ?? ''} ${text}`),
        chunkerVersion: `${TOKEN_FITTED_CHUNKER_VERSION}|${chunk.chunkerVersion ?? CHUNKER_VERSION}`,
        embeddingProjection });
      startLine = endLine;
      previousEnd = segment.end;
      if (fitted.length > 40000) throw retrievalFailure('Token fitting exceeds the source block budget. / token 分块超过来源块预算。', 'RETRIEVAL_SOURCE_TOO_LARGE');
    }
    if (previousEnd !== chunk.text.length)
      throw retrievalFailure('Token fitting omitted original text. / token 分块遗漏原文。', 'INVALID_RETRIEVAL_TOKEN_FIT');
  }
  return fitted;
}

export function fittedEmbeddingText(chunk) {
  const projection = validateEmbeddingProjection(chunk.embeddingProjection);
  if (typeof chunk.text !== 'string' || projection.context.length + chunk.text.length > 16384)
    throw retrievalFailure('Fitted embedding input exceeds its character budget. / token 投影超过字符安全预算。', 'INVALID_RETRIEVAL_TOKEN_FIT');
  return projection.context + chunk.text;
}

export function embeddingInputForChunk(source, chunk, options) {
  return chunk.embeddingProjection === undefined ? embeddingTextForChunk(source, chunk, options) : fittedEmbeddingText(chunk);
}
