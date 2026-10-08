export const DOCUMENT_EXTRACTION_VERSIONS = Object.freeze({
  pdf: 'pdfjs-dist-6.4.299:text-v1',
  docx: 'mammoth-1.13.0:yauzl-3.4.0:text-v1'
});

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
