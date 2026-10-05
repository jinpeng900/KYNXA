import { hashText, validateSource } from './retrieval-contracts.mjs';

export const TOKENIZER_VERSION = 'han-bigram-code-tf-v2';
export const CHUNKER_VERSION = 'structure-lines-v1';
export const EMBEDDING_TEXT_VERSION = 'source-context-v1';
const MAX_CHUNK_CHARACTERS = 384;
const MAX_EMBEDDING_CONTEXT_CHARACTERS = 128;
const chunkSections = new WeakMap();

/** Deterministic Han bigrams and identifier terms; original text is never rewritten.
 * 确定性中文二元词与代码标识符分词，始终保留原文。 */
export function lexicalTerms(text, maximumTerms = 20000) {
  return collectLexicalTerms(text, maximumTerms, true);
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
export function embeddingTextForChunk(source, chunk, { maxChars = 512 } = {}) {
  if (!Number.isSafeInteger(maxChars) || maxChars < 64 || maxChars > 8000) throw new RangeError('Invalid embedding text size.');
  const body = typeof chunk?.text === 'string' ? chunk.text : '';
  const offset = (Number.isSafeInteger(chunk?.startOffset) ? chunk.startOffset : 0) + Math.max(0, body.search(/\S/u));
  const metadata = [];
  const metadataValue = value => truncateCharacters(value.replace(/[\r\n\x00-\x1f]/g, ' ').trim(), 32);
  if (typeof source?.title === 'string' && source.title.trim()) metadata.push(`Title: ${metadataValue(source.title)}`);
  if (typeof source?.locator?.relativePath === 'string' && source.locator.relativePath.trim())
    metadata.push(`Path: ${metadataValue(source.locator.relativePath)}`);
  if (typeof source?.text === 'string') {
    const prepared = chunkSections.get(chunk);
    let section = prepared?.text === source.text ? prepared.sectionTitle : undefined;
    // Generated chunks carry a linear heading map; supplied chunks fall back to their real source text.
    // 自有分块携带线性扫描得到的小节映射；外部传入分块则从真实原文补查，避免逐块扫描整份长文档。
    if (section === undefined) {
      for (const heading of source.text.matchAll(/^#{1,6}\s+(.+)$/gm)) {
        if (heading.index > offset) break;
        section = heading[1].trim();
      }
    }
    if (typeof section === 'string' && section.trim()) metadata.push(`Section: ${metadataValue(section)}`);
  }
  // Reserve the original chunk first. Character bounds supplement, not replace, model token limits.
  // 优先给原始分块预留空间；字符上限只补充约束，不能替代模型 tokenizer 的 token 限制。
  const contextBudget = Math.min(MAX_EMBEDDING_CONTEXT_CHARACTERS, Math.max(0, maxChars - body.length - 2));
  const context = truncateCharacters(metadata.join('\n').replace(/[\r\x00-\x08\x0b\x0c\x0e-\x1f]/g, ' '), contextBudget);
  return truncateCharacters(context ? `${context}\n\n${body}` : body, maxChars);
}

export function matchExpression(query) {
  // User punctuation never becomes executable FTS query syntax.
  // 用户的标点不作为 FTS 查询运算符执行。
  const terms = lexicalTerms(query, 64);
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
