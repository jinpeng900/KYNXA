import { hashText, validateSource, retrievalFailure } from './retrieval-contracts.mjs';

export const TOKENIZER_VERSION = 'han-bigram-code-tf-v2';
export const CHUNKER_VERSION = 'structure-lines-v1';
export const EMBEDDING_TEXT_VERSION = 'source-context-v1';
export const STRUCTURED_CHUNKER_VERSION = 'structured-units-v1';
export const STRUCTURED_EMBEDDING_TEXT_VERSION = 'structure-context-v1';
const MAX_CHUNK_CHARACTERS = 384;
const MAX_EMBEDDING_CONTEXT_CHARACTERS = 128;
const chunkSections = new WeakMap();
const sourceHeadingMaps = new WeakMap();
const TECHNICAL_QUERY_CONCEPTS = [
  [['取消', '中断', '终止'], ['cancel', 'cancellation', 'cancelled', 'abort', 'aborted', 'terminate']],
  [['恢复', '续传'], ['resume', 'resumed', 'restore', 'recovery']],
  [['重试'], ['retry', 'retries']],
  [['超时'], ['timeout', 'timedout']],
  [['并发'], ['concurrency', 'concurrent']],
  [['队列'], ['queue', 'queued']],
  [['检查点', '断点'], ['checkpoint', 'checkpoints']],
  [['索引'], ['index', 'indexing', 'indexed']],
  [['缓存'], ['cache', 'cached', 'caching']],
  [['权限', '审批'], ['permission', 'permissions', 'approval', 'authorize', 'authorization']],
  [['绑定', '挂载'], ['binding', 'bound', 'mount', 'mounted']],
  [['持久化', '存储', '保存'], ['persist', 'persisted', 'persistence', 'storage', 'store', 'save', 'saved']],
  [['序列化'], ['serialize', 'serialization']],
  [['压缩'], ['compact', 'compaction', 'compression']],
  [['分块'], ['chunk', 'chunks', 'chunking']],
  [['分词'], ['tokenizer', 'tokenization']],
  [['嵌入'], ['embedding', 'embeddings']],
  [['重排'], ['rerank', 'reranking']],
  [['上下文'], ['context']],
  [['引用', '调用方'], ['reference', 'references', 'caller', 'callers']],
  [['配置', '设置'], ['configuration', 'config', 'settings']],
  [['内存'], ['memory']],
  [['磁盘'], ['disk']],
];

/** Deterministic Han bigrams and identifier terms; original text is never rewritten.
 * 确定性中文二元词与代码标识符分词，始终保留原文。 */
export function lexicalTerms(text, maximumTerms = 20000) {
  return collectLexicalTerms(text, maximumTerms, true);
}

/** Small technical language bridges expand queries only; originals, hashes and source token frequency stay unchanged.
 * 少量技术词语言桥接只扩展查询，原文、哈希和来源词频始终保持不变，不冒充通用翻译。
 */
export function retrievalQueryTerms(text, { maximumTerms = 64, domain = 'mixed' } = {}) {
  const terms = lexicalTerms(text, maximumTerms), seen = new Set(terms);
  for (const aliases of retrievalQueryConcepts(text, { domain })) for (const alias of aliases) {
    if (terms.length >= maximumTerms) return terms;
    if (!seen.has(alias)) { seen.add(alias); terms.push(alias); }
  }
  return terms;
}

export function retrievalQueryConcepts(text, { domain = 'mixed' } = {}) {
  if (domain === 'knowledge') return [];
  const input = String(text).normalize('NFKC'), terms = new Set(lexicalTerms(input, 512));
  return TECHNICAL_QUERY_CONCEPTS.filter(([phrases, aliases]) =>
    phrases.some(phrase => input.includes(phrase)) || aliases.some(alias => terms.has(alias))).map(([, aliases]) => [...aliases]);
}

function collectLexicalTerms(text, maximumTerms, shouldDeduplicate) {
  const terms = [], seen = new Set();
  const append = term => {
    if (terms.length >= maximumTerms || shouldDeduplicate && seen.has(term)) return;
    terms.push(term);
    if (shouldDeduplicate) seen.add(term);
  };
  // Separate Han from adjacent Latin identifiers (for example, SQLite索引).
  // 将相邻英文与中文分开，避免 SQLite索引 这样的混写漏掉中文检索词。
  const tokenInput = String(text).replace(/(\p{Script=Han}+)/gu, ' $1 ');
  for (const match of tokenInput.matchAll(/\p{Script=Han}+|[\p{L}\p{N}_$]+/gu)) {
    const token = match[0];
    if (/^\p{Script=Han}+$/u.test(token)) {
      const characters = [...token];
      for (let index = 0; index < characters.length; index++) {
        append(characters[index]);
        if (index + 1 < characters.length) append(characters[index] + characters[index + 1]);
        if (terms.length >= maximumTerms) break;
      }
    } else {
      const normalized = token.toLowerCase();
      append(normalized);
      const parts = token.replace(/([a-z\d])([A-Z])/g, '$1 $2').replace(/([A-Z])([A-Z][a-z])/g, '$1 $2').split(/[_$\s]+/);
      // Decompose an identifier once per occurrence; plain words must not be counted twice.
      // 每次标识符出现只展开一次；普通单词不能因整词与拆词相同而被重复计数。
      for (const part of new Set(parts.filter(Boolean).map(value => value.toLowerCase()))) {
        if (part !== normalized) append(part);
      }
    }
    if (terms.length >= maximumTerms) break;
  }
  return terms;
}

// Documents retain term frequency for BM25; query terms remain unique and bounded.
// 文档保留 BM25 所需词频；查询词单独去重并限制数量。
export function lexicalText(text) { return collectLexicalTerms(text, 20000, false).join(' '); }

function truncateCharacters(text, maximumCharacters) {
  let end = Math.min(text.length, maximumCharacters);
  if (end < text.length && /[\uD800-\uDBFF]/u.test(text[end - 1])) end--;
  return text.slice(0, end);
}

/** Build bounded embedding context from actual source metadata without changing cited text.
 * 仅从真实来源元信息构建有界嵌入上下文，引用正文、哈希与偏移保持原样。 */
export function embeddingProjectionForChunk(source, chunk, { maxChars = 512 } = {}) {
  if (!Number.isSafeInteger(maxChars) || maxChars < 64 || maxChars > 8000) throw new RangeError('Invalid embedding text size.');
  const body = typeof chunk?.text === 'string' ? chunk.text : '';
  const offset = (Number.isSafeInteger(chunk?.startOffset) ? chunk.startOffset : 0) + Math.max(0, body.search(/\S/u));
  const metadata = [];
  const metadataValue = value => truncateCharacters(value.replace(/[\r\n\x00-\x1f]/g, ' ').trim(), 32);
  if (typeof source?.title === 'string' && source.title.trim()) metadata.push(`Title: ${metadataValue(source.title)}`);
  if (typeof source?.locator?.relativePath === 'string' && source.locator.relativePath.trim())
    metadata.push(`Path: ${metadataValue(source.locator.relativePath)}`);
  if (chunk?.structure) {
    const structuralMetadata = [];
    if (typeof chunk.structure.sectionTitle === 'string' && chunk.structure.sectionTitle.trim())
      structuralMetadata.push(`Section: ${metadataValue(chunk.structure.sectionTitle)}`);
    if (typeof chunk.structure.symbolName === 'string' && chunk.structure.symbolName.trim())
      structuralMetadata.push(`Symbol: ${metadataValue(chunk.structure.symbolName)}`);
    else if (typeof chunk.structure.qualifiedName === 'string' && chunk.structure.qualifiedName.trim())
      structuralMetadata.push(`Symbol: ${metadataValue(chunk.structure.qualifiedName)}`);
    if (typeof chunk.structure.parentSymbol === 'string' && chunk.structure.parentSymbol.trim())
      structuralMetadata.push(`Parent: ${metadataValue(chunk.structure.parentSymbol)}`);
    if (chunk.structure.domain === 'code') metadata.unshift(...structuralMetadata);
    else metadata.push(...structuralMetadata);
  }
  if (!chunk?.structure && typeof source?.text === 'string') {
    const prepared = chunkSections.get(chunk);
    let section = prepared?.text === source.text ? prepared.sectionTitle : undefined;
    // Generated chunks carry a linear heading map; supplied chunks fall back to their real source text.
    // 自有分块携带线性扫描得到的小节映射；外部传入分块则从真实原文补查，避免逐块扫描整份长文档。
    if (section === undefined) {
      // Worker-cloned chunks cannot carry WeakMap entries; cache headings once per source instead.
      // worker 克隆的分块不携带 WeakMap 信息，标题映射按来源只扫描一次，避免版本计算退化为平方复杂度。
      let headingMap = sourceHeadingMaps.get(source);
      if (headingMap?.text !== source.text) {
        headingMap = { text: source.text, headings: [...source.text.matchAll(/^#{1,6}\s+(.+)$/gm)] };
        sourceHeadingMaps.set(source, headingMap);
      }
      let lower = 0, upper = headingMap.headings.length;
      while (lower < upper) {
        const middle = Math.floor((lower + upper) / 2);
        if (headingMap.headings[middle].index <= offset) lower = middle + 1;
        else upper = middle;
      }
      section = lower > 0 ? headingMap.headings[lower - 1][1].trim() : undefined;
    }
    if (typeof section === 'string' && section.trim()) metadata.push(`Section: ${metadataValue(section)}`);
  }
  // Reserve the original chunk first. Character bounds supplement, not replace, model token limits.
  // 优先给原始分块预留空间；字符上限只补充约束，不能替代模型 tokenizer 的 token 限制。
  const contextBudget = Math.min(MAX_EMBEDDING_CONTEXT_CHARACTERS, Math.max(0, maxChars - body.length - 2));
  const context = truncateCharacters(metadata.join('\n').replace(/[\r\x00-\x08\x0b\x0c\x0e-\x1f]/g, ' '), contextBudget);
  return { context: context ? `${context}\n\n` : '', text: body };
}

export function embeddingTextForChunk(source, chunk, { maxChars = 512 } = {}) {
  const projection = embeddingProjectionForChunk(source, chunk, { maxChars });
  return truncateCharacters(projection.context + projection.text, maxChars);
}

export function matchExpression(query, options = {}) {
  // User punctuation never becomes executable FTS query syntax.
  // 用户的标点不作为 FTS 查询运算符执行。
  const terms = retrievalQueryTerms(query, { ...options, maximumTerms: 64 });
  const hasHanBigrams = terms.some(term => /^\p{Script=Han}{2}$/u.test(term));
  return terms.filter(term => !hasHanBigrams || !/^\p{Script=Han}$/u.test(term))
    .map(term => `"${term.replace(/"/g, '""')}"`).join(' OR ');
}

/** Split at headings, paragraph and recognizable code boundaries without claiming AST parsing.
 * 按标题、段落和可识别的代码边界分块，不将正则边界冒称完整 AST 解析。 */
export function chunkSource(input, { maxChars = MAX_CHUNK_CHARACTERS } = {}) {
  const source = validateSource(input), text = source.text;
  if (!Number.isSafeInteger(maxChars) || maxChars < 64 || maxChars > 8000) throw new RangeError('Invalid chunk size.');
  if (!text.length) return [];
  const starts = [0];
  for (const match of text.matchAll(/^#{1,6}\s+|^(?:export\s+)?(?:async\s+)?(?:function|class|interface|def)\s+|\n\s*\n/gm)) {
    if (match.index > starts.at(-1)) starts.push(match.index);
  }
  starts.push(text.length);
  const chunks = [];
  const headings = [...text.matchAll(/^#{1,6}\s+(.+)$/gm)];
  let headingIndex = 0, sectionTitle = null;
  let start = 0, startLine = 1;
  const append = end => {
    if (end <= start) return;
    const content = text.slice(start, end), index = chunks.length;
    const endLine = startLine + (content.match(/\n/g)?.length ?? 0);
    if (!content.trim()) { start = end; startLine = endLine; return; }
    const contentStart = start + Math.max(0, content.search(/\S/u));
    while (headingIndex < headings.length && headings[headingIndex].index <= contentStart) {
      sectionTitle = headings[headingIndex++][1].trim();
    }
    const chunkHash = hashText(content);
    chunks.push({ chunkId: `${hashText(source.sourceId).slice(0, 24)}:${chunkHash.slice(0, 24)}:${index}`,
      chunkIndex: index, text: content, lexicalText: lexicalText(`${source.title} ${source.locator.relativePath ?? ''} ${content}`),
      chunkHash, startOffset: start, endOffset: end,
      startLine, endLine, chunkerVersion: CHUNKER_VERSION,
      tokenizerVersion: TOKENIZER_VERSION });
    chunkSections.set(chunks.at(-1), { text, sectionTitle });
    start = end;
    startLine = endLine;
  };
  for (const boundary of starts) {
    while (boundary - start > maxChars) {
      let hardEnd = start + maxChars;
      if (/[\uD800-\uDBFF]/.test(text[hardEnd - 1])) hardEnd--;
      const newline = text.lastIndexOf('\n', hardEnd);
      append(newline > start + maxChars / 2 ? newline + 1 : hardEnd);
    }
    if (boundary - start > maxChars / 2) append(boundary);
  }
  append(text.length);
  return chunks;
}

function splitsSurrogatePair(text, offset) {
  return offset > 0 && offset < text.length && /[\uD800-\uDBFF]/u.test(text[offset - 1]) &&
    /[\uDC00-\uDFFF]/u.test(text[offset]);
}

/** Assemble bounded chunks within real parser units, adding honest context for unparsed gaps.
 * 在真实解析单元内组装有界分块，未被解析器覆盖的原文以明确的上下文块补齐。 */
export function chunkStructuredSource(input, structure, { maxChars = MAX_CHUNK_CHARACTERS, checkCancelled } = {}) {
  const source = validateSource(input), text = source.text;
  if (!Number.isSafeInteger(maxChars) || maxChars < 64 || maxChars > 8000) throw new RangeError('Invalid chunk size.');
  if (!structure || !['code', 'knowledge'].includes(structure.domain) ||
      !(structure.language === null || typeof structure.language === 'string' && structure.language.length <= 64) ||
      !['parsed', 'partial', 'unavailable'].includes(structure.parseStatus) || !Array.isArray(structure.units))
    throw retrievalFailure('Invalid parsed source structure. / 来源解析结构无效。', 'INVALID_RETRIEVAL_STRUCTURE');
  checkCancelled?.();
  for (const unit of structure.units) if (!unit || typeof unit !== 'object' || Array.isArray(unit))
    throw retrievalFailure('Invalid parser unit. / 解析单元无效。', 'INVALID_RETRIEVAL_STRUCTURE');
  const units = [...structure.units].sort((left, right) => left.startOffset - right.startOffset || left.endOffset - right.endOffset);
  const chunks = [];
  const sourcePrefix = hashText(source.sourceId).slice(0, 24);
  let cursor = 0, lineOffset = 0, lineNumber = 1;
  const lineAt = offset => {
    for (; lineOffset < offset; lineOffset++) if (text[lineOffset] === '\n') lineNumber++;
    return lineNumber;
  };
  const metadataFor = unit => {
    // A parent may own disjoint fragments while its read-back range still spans the real full declaration.
    // 父级可拥有不重叠片段，回读范围仍指向真实完整声明，不扩大实际分块的正文范围。
    const unitStartOffset = unit.unitStartOffset === undefined ? unit.startOffset : unit.unitStartOffset;
    const unitEndOffset = unit.unitEndOffset === undefined ? unit.endOffset : unit.unitEndOffset;
    if (!Number.isSafeInteger(unitStartOffset) || !Number.isSafeInteger(unitEndOffset) || unitStartOffset < 0 ||
        unitStartOffset > unit.startOffset || unitEndOffset < unit.endOffset || unitEndOffset > text.length ||
        unitEndOffset <= unitStartOffset || splitsSurrogatePair(text, unitStartOffset) || splitsSurrogatePair(text, unitEndOffset))
      throw retrievalFailure('Declaration range does not contain its fragment. / 声明完整范围没有覆盖片段。', 'INVALID_RETRIEVAL_STRUCTURE');
    const metadata = { domain: structure.domain, language: structure.language, kind: unit.kind,
      unitStartOffset, unitEndOffset, parseStatus: structure.parseStatus };
    for (const key of ['symbolName', 'qualifiedName', 'parentSymbol', 'sectionTitle']) {
      const value = unit[key];
      if (value === undefined) continue;
      if (typeof value !== 'string' || value.length > 512 || /[\x00-\x1f]/u.test(value))
        throw retrievalFailure('Invalid structural label. / 结构标签无效。', 'INVALID_RETRIEVAL_STRUCTURE');
      if (value.trim()) metadata[key] = value;
    }
    if (unit.sectionPath !== undefined) {
      if (!Array.isArray(unit.sectionPath) || unit.sectionPath.length > 6 ||
          unit.sectionPath.some(value => typeof value !== 'string' || value.length > 512 || /[\x00-\x1f]/u.test(value)))
        throw retrievalFailure('Invalid section hierarchy. / 章节层级无效。', 'INVALID_RETRIEVAL_STRUCTURE');
      metadata.sectionPath = [...unit.sectionPath];
    }
    return metadata;
  };
  const appendUnit = unit => {
    const metadata = metadataFor(unit);
    if (unit.startLine !== undefined && unit.startLine !== lineAt(unit.startOffset))
      throw retrievalFailure('Parser fragment line does not match raw text. / 解析片段起始行与原文不符。', 'INVALID_RETRIEVAL_STRUCTURE');
    let start = unit.startOffset;
    while (start < unit.endOffset) {
      checkCancelled?.();
      let end = Math.min(unit.endOffset, start + maxChars);
      if (splitsSurrogatePair(text, end)) end--;
      if (end < unit.endOffset) {
        // Search only this bounded slice; scanning back to zero for every chunk is quadratic.
        // 只在当前有界片段内查换行，逐块向文档起点反向查找会退化为平方复杂度。
        const newline = text.slice(start, end).lastIndexOf('\n');
        if (newline > maxChars / 2) end = start + newline + 1;
      }
      const startLine = lineAt(start), endLine = lineAt(end);
      const content = text.slice(start, end);
      if (content.trim()) {
        const chunkHash = hashText(content), chunkIndex = chunks.length;
        const structureHash = hashText(JSON.stringify(metadata)).slice(0, 16);
        chunks.push({ chunkId: `${sourcePrefix}:${chunkHash.slice(0, 24)}:${structureHash}:${chunkIndex}`,
          chunkIndex, text: content, chunkHash, startOffset: start, endOffset: end, startLine, endLine,
          lexicalText: lexicalText(`${source.title} ${source.locator.relativePath ?? ''} ${content}`),
          chunkerVersion: STRUCTURED_CHUNKER_VERSION, tokenizerVersion: TOKENIZER_VERSION,
          structure: { ...metadata } });
      }
      start = end;
    }
    if (unit.endLine !== undefined && unit.endLine !== lineAt(unit.endOffset))
      throw retrievalFailure('Parser fragment line does not match raw text. / 解析片段结束行与原文不符。', 'INVALID_RETRIEVAL_STRUCTURE');
  };
  for (const unit of units) {
    checkCancelled?.();
    if (!unit || !Number.isSafeInteger(unit.startOffset) || !Number.isSafeInteger(unit.endOffset) ||
        unit.startOffset < cursor || unit.endOffset <= unit.startOffset || unit.endOffset > text.length ||
        splitsSurrogatePair(text, unit.startOffset) || splitsSurrogatePair(text, unit.endOffset) ||
        typeof unit.kind !== 'string' || !/^[a-z][a-z-]{0,63}$/u.test(unit.kind))
      throw retrievalFailure('Parser unit ranges overlap or mismatch the source. / 解析单元范围重叠或与原文不符。', 'INVALID_RETRIEVAL_STRUCTURE');
    if (unit.startOffset > cursor) appendUnit({ kind: 'context', startOffset: cursor, endOffset: unit.startOffset });
    appendUnit(unit);
    cursor = unit.endOffset;
  }
  if (cursor < text.length) appendUnit({ kind: 'context', startOffset: cursor, endOffset: text.length });
  checkCancelled?.();
  return chunks;
}
