import { retrievalFailure } from './retrieval-contracts.mjs';

/** Failed extraction may describe decoded work, but publishes no pages and never supplies source authority.
 * 失败提取可以描述已解码工作，但没有发布页面，也不能由诊断赋予来源权限。
 */
export function validateDocumentCoverage(value) {
  const fields = ['format', 'reasonCode', 'complete', 'processedPages', 'pageCount', 'publishedPages', 'rawContentHash',
    'uncoveredRanges', 'unprocessedRanges', 'missingTextPages', 'effectiveLimits', 'limit', 'readback'];
  const fail = () => { throw retrievalFailure('Invalid document coverage. / 文档覆盖诊断无效。', 'INVALID_RETRIEVAL_COVERAGE'); };
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !fields.includes(key)) ||
      !['pdf', 'docx'].includes(value.format) || !/^(?:DOCUMENT_[A-Z_]+|OCR_UNAVAILABLE)$/u.test(value.reasonCode ?? '') ||
      value.complete !== false || value.publishedPages !== 0 || !Number.isSafeInteger(value.processedPages) || value.processedPages < 0 ||
      value.rawContentHash !== undefined && !/^[a-f0-9]{64}$/u.test(value.rawContentHash)) fail();
  if (value.pageCount !== undefined && (!Number.isSafeInteger(value.pageCount) || value.pageCount < 1 || value.processedPages > value.pageCount)) fail();
  if (value.format === 'docx' && (value.pageCount !== undefined || value.processedPages !== 0)) fail();
  for (const key of ['uncoveredRanges', 'unprocessedRanges']) {
    if (!Array.isArray(value[key]) || value[key].length > 101 || value[key].some(range => !range ||
        Object.keys(range).some(name => !['startPage', 'endPage'].includes(name)) ||
        !Number.isSafeInteger(range.startPage) || !Number.isSafeInteger(range.endPage) || range.startPage < 1 ||
        range.endPage < range.startPage || value.pageCount === undefined || range.endPage > value.pageCount)) fail();
  }
  if (value.missingTextPages !== undefined && (!Array.isArray(value.missingTextPages) || value.missingTextPages.length > 100 ||
      value.missingTextPages.some(page => !Number.isSafeInteger(page) || page < 1 || page > value.pageCount))) fail();
  const limits = value.effectiveLimits;
  const limitFields = ['maximumInputBytes', 'maximumOutputBytes', 'maximumPages'];
  if (!limits || Object.keys(limits).some(key => !limitFields.includes(key)) ||
      limitFields.some(key => !Number.isSafeInteger(limits[key]) || limits[key] < 1)) fail();
  if (value.limit !== undefined && (!value.limit || Object.keys(value.limit).some(key => !['dimension', 'limit', 'observed'].includes(key)) ||
      !['maximumInputBytes', 'maximumOutputBytes', 'maximumPages', 'maximumExpandedBytes', 'maximumArchiveEntries'].includes(value.limit.dimension) ||
      !Number.isSafeInteger(value.limit.limit) || value.limit.limit < 1 || !Number.isSafeInteger(value.limit.observed) || value.limit.observed < 0)) fail();
  const readback = value.readback;
  if (!readback || Object.keys(readback).some(key => !['kind', 'startPage', 'endPage'].includes(key)) || readback.kind !== 'original-document' ||
      readback.startPage !== undefined && (!Number.isSafeInteger(readback.startPage) || readback.startPage < 1) ||
      readback.endPage !== undefined && (!Number.isSafeInteger(readback.endPage) || readback.endPage < readback.startPage || readback.endPage > value.pageCount)) fail();
  return structuredClone(value);
}
