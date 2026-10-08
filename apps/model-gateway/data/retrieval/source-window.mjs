import { MAX_SOURCE_CHARACTERS, retrievalFailure } from './retrieval-contracts.mjs';
import { parseDocumentStructure } from './document-structure.mjs';

export const MAX_SOURCE_WINDOW_CHARACTERS = 16000;
export const SOURCE_WINDOW_VERSION = 'source-window-v1';
const integer = (value, minimum, maximum) => Number.isSafeInteger(value) && value >= minimum && value <= maximum;
const splitsPair = (text, offset) => offset > 0 && offset < text.length &&
  /[\uD800-\uDBFF]/.test(text[offset - 1]) && /[\uDC00-\uDFFF]/.test(text[offset]);

export function validateSourceWindowOptions({ mode = 'window', anchorOffset, beforeCharacters = 384, limit = 4000 } = {}) {
  if (!['window', 'section'].includes(mode) || (anchorOffset !== undefined && !integer(anchorOffset, 0, MAX_SOURCE_CHARACTERS)) ||
      !integer(beforeCharacters, 0, 4096) || !integer(limit, 2, MAX_SOURCE_WINDOW_CHARACTERS))
    throw retrievalFailure('Invalid source window. / 资料回读窗口参数无效。', 'INVALID_RETRIEVAL_WINDOW');
  return { mode, anchorOffset, beforeCharacters, limit };
}

/** Heading offsets point into the original UTF-16 text; fenced code cannot create a chapter.
 * 标题偏移直接指向原始 UTF-16 正文，围栏代码里的标题不形成章节。 */
function markdownStructure(text, checkCancelled) {
  const parsed = parseDocumentStructure({ text, locator: { relativePath: 'source.md' } }, { checkCancelled });
  const headings = parsed.units
    .filter(unit => unit.kind === 'heading')
    .map(unit => ({ title: unit.sectionTitle, level: unit.headingLevel, startOffset: unit.startOffset }));
  return { headings, incomplete: parsed.diagnosticCodes.includes('DOCUMENT_STRUCTURE_LIMIT'), diagnosticCodes: parsed.diagnosticCodes };
}

/** Return bounded original text around an anchor, optionally inside its real Markdown section.
 * 围绕锚点返回有界原文，可限制在所属真实 Markdown 章节内；不重排、重写或重建分块。 */
export function sourceWindow(text, options = {}, checkCancelled) {
  if (typeof text !== 'string' || text.length > MAX_SOURCE_CHARACTERS)
    throw retrievalFailure('Invalid source text. / 资料正文无效。', 'INVALID_RETRIEVAL_WINDOW');
  const { mode, beforeCharacters, limit } = validateSourceWindowOptions(options);
  const anchorOffset = options.anchorOffset ?? 0;
  if (anchorOffset > text.length || splitsPair(text, anchorOffset))
    throw retrievalFailure('Invalid source anchor. / 回读锚点越界或位于字符中间。', 'INVALID_RETRIEVAL_OFFSET');
  checkCancelled?.();
  const referenceRange = options.referenceRange;
  if (referenceRange && (!integer(referenceRange.startOffset, 0, text.length) ||
      !integer(referenceRange.endOffset, referenceRange.startOffset + 1, text.length) ||
      splitsPair(text, referenceRange.startOffset) || splitsPair(text, referenceRange.endOffset)))
    throw retrievalFailure('Invalid chunk range. / 分块范围无效。', 'INVALID_RETRIEVAL_WINDOW');
  let section, sections, sectionDiagnostics, spansSections = false, navigationTruncated = false, lower = 0, upper = text.length;
  if (mode === 'section') {
    const parsed = markdownStructure(text, checkCancelled);
    // Partial navigation cannot prove the final chapter boundary; retain a truthful raw window.
    // 部分章节导航无法证明最终边界，此时回退为真实原文窗口，不把未解析的后续标题归入前章。
    const headings = parsed.incomplete ? [] : parsed.headings;
    if (parsed.incomplete) sectionDiagnostics = parsed.diagnosticCodes;
    let selected = -1;
    for (let index = 0; index < headings.length; index++) {
      if (headings[index].startOffset > anchorOffset) break;
      selected = index;
    }
    if (selected >= 0) {
      const heading = headings[selected];
      const next = headings.slice(selected + 1).find(item => item.level <= heading.level);
      lower = heading.startOffset; upper = next?.startOffset ?? text.length;
      section = { ...heading, endOffset: upper };
    }
    // A chunk can straddle headings. Preserve its entire range instead of guessing one chapter.
    // 分块可能跨标题；此时保留整个引用范围，不猜测它只属于某一章。
    if (referenceRange) {
      const crossed = headings.filter(heading => heading.startOffset > referenceRange.startOffset && heading.startOffset < referenceRange.endOffset);
      spansSections = crossed.length > 0;
      if (spansSections) {
        const navigation = [...(selected >= 0 ? [headings[selected]] : []), ...crossed];
        navigationTruncated = navigation.length > 16;
        sections = navigation.slice(0, 16).map(heading => ({ ...heading,
          endOffset: headings.find(next => next.startOffset > heading.startOffset && next.level <= heading.level)?.startOffset ?? text.length }));
        section = undefined; lower = 0; upper = text.length;
      }
    }
  }
  let startOffset = Math.max(lower, anchorOffset - Math.min(beforeCharacters, limit - 2));
  if (section && upper - lower <= limit) startOffset = lower;
  if (spansSections && referenceRange.endOffset - referenceRange.startOffset <= limit)
    startOffset = Math.max(startOffset, referenceRange.endOffset - limit);
  if (splitsPair(text, startOffset)) startOffset--;
  let endOffset = Math.min(upper, startOffset + limit);
  if (splitsPair(text, endOffset)) endOffset--;
  checkCancelled?.();
  return { text: text.slice(startOffset, endOffset), offset: startOffset, nextOffset: endOffset, offsetUnit: 'utf16-code-units',
    totalCharacters: text.length, hasMore: endOffset < upper,
    window: { version: SOURCE_WINDOW_VERSION, mode: section ? 'section' : 'window', anchorOffset, offsetUnit: 'utf16-code-units',
      startOffset, endOffset, clippedAtStart: startOffset > lower, clippedAtEnd: endOffset < upper,
      ...(section ? { section } : {}), ...(sections ? { sections, spansSections, navigationTruncated } : {}),
      ...(sectionDiagnostics ? { sectionUnavailable: true, navigationTruncated: true, diagnosticCodes: sectionDiagnostics } : {}),
      ...(referenceRange ? { referenceRange: { ...referenceRange },
        referenceRangeCovered: startOffset <= referenceRange.startOffset && endOffset >= referenceRange.endOffset } : {}) } };
}
