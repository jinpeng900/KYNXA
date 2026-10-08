export const EMBEDDING_DOCUMENT_FITTING_VERSION = 'e5-document-token-fit-v1';
export const MAX_EMBEDDING_INPUT_CHARACTERS = 16_384;

function fittingError(message, code, details) {
  return Object.assign(new Error(message), { code, details });
}

function preferredEnd(text, start, end) {
  const minimumEnd = start + Math.floor((end - start) * 0.8);
  const tail = text.slice(minimumEnd, end);
  const boundaries = [...tail.matchAll(/[\n。！？.!?](?:\s|$)|\s+/gu)];
  const boundary = boundaries.at(-1);
  return boundary ? minimumEnd + boundary.index + boundary[0].length : end;
}

// Token measurements use the exact projected input, while every returned range addresses raw UTF-16 text.
// token 测量使用完整实际投影输入，返回范围始终指向原始 UTF-16 正文，不归一化或丢弃任何字符。
export async function fitEmbeddingDocuments(documents, {
  countTokens, documentPrefix, maxInputTokens, checkCancelled = () => {},
  yieldToMessages = () => new Promise(resolve => setImmediate(resolve)),
} = {}) {
  const fittedDocuments = [];
  for (let index = 0; index < documents.length; index += 1) {
    await yieldToMessages();
    checkCancelled();
    const { text, context } = documents[index];
    const measure = (start, end) => {
      checkCancelled();
      return countTokens(`${documentPrefix}${context}${text.slice(start, end)}`);
    };
    const tokenCount = measure(0, text.length);
    const maximumBodyCharacters = MAX_EMBEDDING_INPUT_CHARACTERS - context.length;
    const contextTokenCount = countTokens(`${documentPrefix}${context}`);
    if (maximumBodyCharacters < 1 || contextTokenCount >= maxInputTokens) {
      throw fittingError('Embedding context leaves no room for the document body.', 'EMBEDDING_CONTEXT_TOO_LONG',
        { index, tokenCount: contextTokenCount, maxInputTokens });
    }
    if (tokenCount <= maxInputTokens && text.length <= maximumBodyCharacters) {
      fittedDocuments.push({ tokenCount, segments: [{ start: 0, end: text.length, tokenCount }] });
      continue;
    }

    const offsets = [0];
    let offset = 0;
    for (const character of text) {
      offset += character.length;
      offsets.push(offset);
    }
    const segments = [];
    let startIndex = 0;
    while (startIndex < offsets.length - 1) {
      await yieldToMessages();
      checkCancelled();
      const start = offsets[startIndex];
      let upperIndex = startIndex + 1;
      while (upperIndex + 1 < offsets.length && offsets[upperIndex + 1] - start <= maximumBodyCharacters) upperIndex++;
      if (offsets[upperIndex] - start > maximumBodyCharacters) {
        throw fittingError('Embedding context cannot accommodate a complete body character.', 'EMBEDDING_CONTEXT_TOO_LONG',
          { index, maxInputTokens });
      }

      let bestIndex, bestTokenCount;
      const upperTokenCount = measure(start, offsets[upperIndex]);
      if (upperTokenCount <= maxInputTokens) {
        bestIndex = upperIndex;
        bestTokenCount = upperTokenCount;
      } else {
        let lowerIndex = startIndex + 1, searchUpperIndex = upperIndex - 1;
        while (lowerIndex <= searchUpperIndex) {
          await yieldToMessages();
          checkCancelled();
          const middleIndex = Math.floor((lowerIndex + searchUpperIndex) / 2);
          const candidateTokenCount = measure(start, offsets[middleIndex]);
          if (candidateTokenCount <= maxInputTokens) {
            bestIndex = middleIndex;
            bestTokenCount = candidateTokenCount;
            lowerIndex = middleIndex + 1;
          } else searchUpperIndex = middleIndex - 1;
        }
      }
      if (bestIndex === undefined) {
        throw fittingError('Embedding context cannot accommodate a document body character.', 'EMBEDDING_CONTEXT_TOO_LONG',
          { index, tokenCount: measure(start, offsets[startIndex + 1]), maxInputTokens });
      }

      let end = offsets[bestIndex];
      if (bestIndex < offsets.length - 1) {
        const boundary = preferredEnd(text, start, end);
        if (boundary > start && boundary < end) {
          const boundaryTokenCount = measure(start, boundary);
          // A shorter prefix can tokenize differently; only an explicitly measured valid boundary is accepted.
          // 更短前缀也可能改变分词；只接受经过实际计数且满足预算的边界，不假设 token 数严格单调。
          if (boundaryTokenCount <= maxInputTokens) {
            end = boundary;
            bestTokenCount = boundaryTokenCount;
            while (offsets[bestIndex] > end) bestIndex--;
          }
        }
      }
      segments.push({ start, end, tokenCount: bestTokenCount });
      startIndex = bestIndex;
    }
    fittedDocuments.push({ tokenCount, segments });
  }
  checkCancelled();
  return fittedDocuments;
}
