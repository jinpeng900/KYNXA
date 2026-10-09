export const DOCUMENT_EXTRACTION_VERSIONS = Object.freeze({
  pdf: 'pdfjs-dist-6.4.299:text-v2',
  docx: 'mammoth-1.13.0:yauzl-3.4.0:text-v1'
});

/** Page cursors bind one original byte revision; per-window page budgets remain independent of total page count.
 * 页面游标绑定单份原文件字节版本；单窗口页预算独立于整份 PDF 的总页数。 */
export function validateDocumentPageWindow(value, maximumPages = DOCUMENT_EXTRACTION_LIMITS.maximumPages) {
  if (value === undefined) return undefined;
  if (!value || typeof value !== 'object' || Array.isArray(value) ||
      Object.keys(value).some(key => !['startPage', 'endPage', 'rawContentHash'].includes(key)) ||
      !Number.isSafeInteger(value.startPage) || value.startPage < 1 || value.startPage > 1000000 ||
      value.endPage !== undefined && (!Number.isSafeInteger(value.endPage) || value.endPage < value.startPage ||
        value.endPage > 1000000 || value.endPage - value.startPage + 1 > maximumPages) ||
      value.rawContentHash !== undefined && !/^[a-f0-9]{64}$/u.test(value.rawContentHash))
    throw documentExtractionFailure('DOCUMENT_INVALID_PAGE_WINDOW');
  return { ...value };
}

export const DOCUMENT_EXTRACTION_LIMITS = Object.freeze({
  maximumInputBytes: 32 * 1024 * 1024, maximumOutputBytes: 2 * 1024 * 1024,
  maximumPages: 100, maximumArchiveEntries: 512, maximumExpandedBytes: 16 * 1024 * 1024,
  maximumProcesses: 2, timeoutMs: 10_000, maximumHeapMb: 192, maximumRssBytes: 256 * 1024 * 1024
});

export function documentExtractionFailure(code, details) {
  const statusCode = /(?:LIMIT|BUDGET)$/u.test(code) ? 413 : code === 'DOCUMENT_DECODER_TIMEOUT' ? 504
    : /(?:FAILED|BUSY|UNSUPPORTED)$/u.test(code) && code !== 'DOCUMENT_PARSE_FAILED' ? 503 : 422;
  const message = code === 'OCR_UNAVAILABLE'
    ? 'A PDF page has no extractable text; OCR is unavailable. / PDF 页面没有可提取文本，当前未提供 OCR。'
    : 'Document text extraction failed within its bounded offline decoder. / 有界离线文档文本提取失败。';
  return Object.assign(new Error(message), { code, statusCode, ...(details ? { details } : {}) });
}

export function validateDocumentExtractionLimits(options = {}) {
  if (!options || typeof options !== 'object' || Array.isArray(options) ||
      Object.keys(options).some(key => !Object.hasOwn(DOCUMENT_EXTRACTION_LIMITS, key)))
    throw documentExtractionFailure('DOCUMENT_INVALID_LIMITS');
  const limits = { ...DOCUMENT_EXTRACTION_LIMITS, ...options };
  for (const key of Object.keys(DOCUMENT_EXTRACTION_LIMITS))
    if (!Number.isSafeInteger(limits[key]) || limits[key] < 1 || limits[key] > DOCUMENT_EXTRACTION_LIMITS[key])
      throw documentExtractionFailure('DOCUMENT_INVALID_LIMITS');
  return limits;
}
