import { extname } from 'node:path';
import { MAX_SOURCE_CHARACTERS, retrievalFailure } from './retrieval-contracts.mjs';

export const MARKDOWN_PARSER_VERSION = 'markdown-blocks-v2';
export const TEXT_PARSER_VERSION = 'plain-paragraphs-v2';
const MAX_SECTION_TITLE_CHARACTERS = 128;
const MAX_DOCUMENT_LINES = 100_000;
const MAX_DOCUMENT_UNITS = 5000;
const MAX_PARSE_TIME_MS = 250;
const MARKDOWN_EXTENSIONS = new Set(['.md', '.markdown']);

function boundedTitle(value) {
  const text = value.replace(/[\r\n\t ]+/gu, ' ').trim();
  let end = Math.min(text.length, MAX_SECTION_TITLE_CHARACTERS);
  if (end < text.length && /[\uD800-\uDBFF]/u.test(text[end - 1])) end--;
  return text.slice(0, end);
}

function sourceLines(text, checkProgress) {
  const lines = [];
  let offset = 0, lineNumber = 1;
  while (offset < text.length) {
    if (lines.length >= MAX_DOCUMENT_LINES || lines.length % 256 === 0 && !checkProgress()) break;
    const newline = text.indexOf('\n', offset);
    const endOffset = newline < 0 ? text.length : newline + 1;
    let contentEnd = newline < 0 ? text.length : newline;
    if (contentEnd > offset && text[contentEnd - 1] === '\r') contentEnd--;
    lines.push({ text: text.slice(offset, contentEnd), startOffset: offset, endOffset,
      startLine: lineNumber, endLine: newline < 0 ? lineNumber : lineNumber + 1 });
    offset = endOffset;
    lineNumber++;
  }
  return { lines, complete: offset === text.length };
}

const atxHeading = line => /^ {0,3}(#{1,6})(?:[\t ]+|$)(.*)$/u.exec(line);
const setextHeading = line => /^ {0,3}(=+|-+)[\t ]*$/u.exec(line);
const fenceMarker = line => /^ {0,3}(`{3,}|~{3,})(.*)$/u.exec(line);
const hasTablePipe = line => /(?<!\\)\|/u.test(line);
const tableDelimiter = line => /^ {0,3}\|?[\t ]*:?-+:?[\t ]*(?:\|[\t ]*:?-+:?[\t ]*)+\|?[\t ]*$/u.test(line);
const setextEligible = line => line.trim() && !/^(?: {4}|\t)|^\s*(?:[-+*]\s|\d+[.)]\s|>)/u.test(line);

function markdownSource(source, lines) {
  const path = source.locator?.relativePath ?? source.locator?.path ?? source.locator?.name ?? source.title ?? '';
  const extension = extname(path).toLowerCase();
  if (MARKDOWN_EXTENSIONS.has(extension)) return true;
  if (extension) return false;
  return lines.some((line, index) => atxHeading(line.text) || fenceMarker(line.text) ||
    index > 0 && setextHeading(line.text) && setextEligible(lines[index - 1].text));
}

/** Locate supported document blocks in one linear scan; raw UTF-16 text is never normalized.
 * 以线性扫描定位支持的文档块，始终保留原始 UTF-16 正文、换行与偏移。 */
export function parseDocumentStructure(source, { checkCancelled } = {}) {
  if (!source || typeof source.text !== 'string' || source.text.length > MAX_SOURCE_CHARACTERS)
    throw retrievalFailure('Invalid document source. / 文档正文无效。', 'INVALID_RETRIEVAL_STRUCTURE');
  checkCancelled?.();
  const deadline = performance.now() + MAX_PARSE_TIME_MS;
  const checkProgress = () => { checkCancelled?.(); return performance.now() <= deadline; };
  const scanned = sourceLines(source.text, checkProgress), lines = scanned.lines;
  const markdown = markdownSource(source, lines);
  const units = [], sectionTitles = [], diagnosticCodes = [];
  let limited = !scanned.complete, parseStatus = limited ? 'partial' : 'parsed';
  const append = (kind, startIndex, endIndex, heading) => {
    if (units.length >= MAX_DOCUMENT_UNITS) { limited = true; return; }
    if (heading) {
      sectionTitles.length = heading.level;
      sectionTitles[heading.level - 1] = heading.title;
    }
    const sectionPath = sectionTitles.filter(Boolean);
    const first = lines[startIndex], last = lines[endIndex - 1];
    units.push({ kind, startOffset: first.startOffset, endOffset: last.endOffset,
      startLine: first.startLine, endLine: last.endLine,
      ...(sectionPath.length ? { sectionTitle: sectionPath.at(-1), sectionPath } : {}),
      ...(heading ? { headingLevel: heading.level, sectionTitle: heading.title } : {}) });
  };
  let index = 0;
  while (index < lines.length) {
    if (units.length >= MAX_DOCUMENT_UNITS || !checkProgress()) { limited = true; break; }
    const line = lines[index].text;
    if (!line.trim()) { index++; continue; }
    if (!markdown) {
      const start = index++;
      while (index < lines.length && lines[index].text.trim()) {
        if (index % 256 === 0 && !checkProgress()) { limited = true; break; }
        index++;
      }
      append('paragraph', start, index);
      continue;
    }
    const fence = fenceMarker(line);
    if (fence && !(fence[1][0] === '`' && fence[2].includes('`'))) {
      const start = index++;
      let closed = false;
      for (; index < lines.length; index++) {
        if (!checkProgress()) { limited = true; break; }
        const closing = fenceMarker(lines[index].text);
        if (closing && closing[1][0] === fence[1][0] && closing[1].length >= fence[1].length && !closing[2].trim()) {
          index++; closed = true; break;
        }
      }
      if (!closed) {
        parseStatus = 'partial';
        if (!limited) diagnosticCodes.push('UNTERMINATED_CODE_FENCE');
      }
      append('code-block', start, index);
      continue;
    }
    const heading = atxHeading(line);
    if (heading) {
      append('heading', index, index + 1, { level: heading[1].length,
        title: boundedTitle(heading[2].replace(/[\t ]+#+[\t ]*$/u, '')) });
      index++;
      continue;
    }
    if (index + 1 < lines.length && hasTablePipe(line) && tableDelimiter(lines[index + 1].text)) {
      const start = index;
      index += 2;
      while (index < lines.length && lines[index].text.trim() && hasTablePipe(lines[index].text) &&
          !atxHeading(lines[index].text) && !fenceMarker(lines[index].text)) {
        if (index % 256 === 0 && !checkProgress()) { limited = true; break; }
        index++;
      }
      append('table', start, index);
      continue;
    }
    const start = index++;
    let setext = null;
    while (index < lines.length && lines[index].text.trim()) {
      if (!checkProgress()) { limited = true; break; }
      const current = lines[index].text;
      const marker = setextHeading(current);
      if (marker && setextEligible(lines[index - 1].text)) {
        // Preserve the existing single-line Setext navigation contract, not a full renderer grammar.
        // 保留现有单行 Setext 章节导航约定，不将轻量块定位冒称完整渲染器语法。
        if (index - start > 1) append('paragraph', start, index - 1);
        setext = { level: marker[1][0] === '=' ? 1 : 2,
          title: boundedTitle(lines[index - 1].text), start: index - 1 };
        index++;
        break;
      }
      if (atxHeading(current) || fenceMarker(current) || index + 1 < lines.length &&
          hasTablePipe(current) && tableDelimiter(lines[index + 1].text)) break;
      index++;
    }
    append(setext ? 'heading' : 'paragraph', setext?.start ?? start, index, setext);
  }
  checkCancelled?.();
  // Unparsed suffixes remain raw context chunks upstream; a budget limit never claims full coverage.
  // 超预算后剩余原文由上层作为上下文块保留，不能把部分结构标为完整解析。
  if (limited) { parseStatus = 'partial'; diagnosticCodes.push('DOCUMENT_STRUCTURE_LIMIT'); }
  return { domain: 'knowledge', language: markdown ? 'markdown' : 'text',
    parserVersion: markdown ? MARKDOWN_PARSER_VERSION : TEXT_PARSER_VERSION, parseStatus, diagnosticCodes, units };
}
