import { createRequire, syncBuiltinESMExports } from 'node:module';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import tls from 'node:tls';
import { fileURLToPath } from 'node:url';
import { documentExtractionFailure, validateDocumentExtractionLimits, validateDocumentPageWindow } from './document-extraction-contracts.mjs';

const require = createRequire(import.meta.url);
let started = false;
const ocrReplies = new Map();

// The authorized parent brokers OCR; this restricted decoder cannot spawn a helper or choose a source path.
// 已授权父进程代理 OCR；受限解码器不能启动助手或选择来源路径。
function recognizePage(page) {
  return new Promise((resolve, reject) => {
    ocrReplies.set(page, { resolve, reject });
    process.send?.({ type: 'ocr-request', requestId: page, pages: [page] });
  });
}
const denyNetwork = () => { throw documentExtractionFailure('DOCUMENT_EXTERNAL_RESOURCE'); };
globalThis.fetch = denyNetwork;
globalThis.WebSocket = class { constructor() { denyNetwork(); } };
http.request = http.get = https.request = https.get = denyNetwork;
net.connect = net.createConnection = tls.connect = denyNetwork;
syncBuiltinESMExports();

function requireVersion(packageName, version) {
  let installed;
  try { installed = require(`${packageName}/package.json`).version; }
  catch { throw documentExtractionFailure('DOCUMENT_DECODER_VERSION_UNSUPPORTED'); }
  if (installed !== version)
    throw documentExtractionFailure('DOCUMENT_DECODER_VERSION_UNSUPPORTED');
}

function observeMemory(limits) {
  const rssBytes = process.memoryUsage().rss;
  if (rssBytes > limits.maximumRssBytes) throw documentExtractionFailure('DOCUMENT_DECODER_MEMORY_LIMIT');
  process.send?.({ type: 'memory', rssBytes });
}

async function pdfText(bytes, limits, { pageWindow, ocrEnabled } = {}) {
  requireVersion('pdfjs-dist', '6.4.299');
  let getDocument;
  try { ({ getDocument } = await import('pdfjs-dist/legacy/build/pdf.mjs')); }
  catch { throw documentExtractionFailure('DOCUMENT_DECODER_UNSUPPORTED'); }
  const task = getDocument({ data: new Uint8Array(bytes), disableFontFace: true, useSystemFonts: false,
    cMapUrl: fileURLToPath(new URL('../../node_modules/pdfjs-dist/cmaps/', import.meta.url)).replaceAll('\\', '/'), cMapPacked: true,
    standardFontDataUrl: fileURLToPath(new URL('../../node_modules/pdfjs-dist/standard_fonts/', import.meta.url)).replaceAll('\\', '/'),
    useWasm: false, useWorkerFetch: false, disableRange: true, disableStream: true, disableAutoFetch: true,
    stopAtErrors: true, enableXfa: false, isEvalSupported: false, isOffscreenCanvasSupported: false, isImageDecoderSupported: false,
    maxImageSize: 0, verbosity: 0 });
  let pageCount, requestedWindow, processedPages = 0;
  try {
    const document = await task.promise;
    pageCount = document.numPages;
    const window = validateDocumentPageWindow(pageWindow, limits.maximumPages);
    const startPage = window?.startPage ?? 1, endPage = Math.min(window?.endPage ?? startPage + limits.maximumPages - 1, pageCount);
    requestedWindow = { startPage, endPage };
    if (startPage > pageCount || window?.endPage !== undefined && window.endPage > pageCount)
      throw documentExtractionFailure('DOCUMENT_INVALID_PAGE_WINDOW');
    const pages = [], missingTextPages = [];
    let text = '', ocr;
    for (let page = startPage; page <= endPage; page++) {
      const current = await document.getPage(page);
      const content = await current.getTextContent();
      const pieces = [];
      let pageBytes = 0;
      for (const item of content.items) {
        if (typeof item.str !== 'string') continue;
        const piece = item.str + (item.hasEOL ? '\n' : ' ');
        pageBytes += Buffer.byteLength(piece);
        if (pageBytes > limits.maximumOutputBytes) throw documentExtractionFailure('DOCUMENT_OUTPUT_LIMIT',
          { limit: { dimension: 'maximumOutputBytes', limit: limits.maximumOutputBytes, observed: pageBytes } });
        pieces.push(piece);
      }
      let pageText = pieces.join('').replace(/[ \t]+\n/gu, '\n').trimEnd(), method;
      if (!pageText.trim() && ocrEnabled) {
        let recognized;
        try { recognized = await recognizePage(page); }
        catch (error) { error.pages = [page]; throw error; }
        pageText = recognized.text;
        if (ocr && (ocr.version !== recognized.version || ocr.language !== recognized.language))
          throw documentExtractionFailure('DOCUMENT_OCR_IDENTITY_CHANGED');
        ocr = { backend: 'windows-media-ocr', version: recognized.version, language: recognized.language };
        method = 'ocr';
      }
      if (!pageText.trim() && method !== 'ocr') missingTextPages.push(page);
      const separator = pages.length ? '\n\n' : '';
      if (pages.length && Buffer.byteLength(text + separator + pageText) > limits.maximumOutputBytes) { current.cleanup(); break; }
      text += separator;
      const startOffset = text.length;
      text += pageText;
      pages.push({ page, startOffset, endOffset: text.length, ...(method ? { method } : {}) });
      if (Buffer.byteLength(text) > limits.maximumOutputBytes) throw documentExtractionFailure('DOCUMENT_OUTPUT_LIMIT',
        { limit: { dimension: 'maximumOutputBytes', limit: limits.maximumOutputBytes, observed: Buffer.byteLength(text) } });
      current.cleanup();
      processedPages = pages.length;
      observeMemory(limits);
      // OCR rendering has a separate native memory footprint; keep image-heavy windows short and resumable.
      // OCR 渲染另占原生内存；图像页窗口保持短且可继续，避免单轮堆积整份扫描 PDF。
      if (ocr && pages.filter(item => item.method === 'ocr').length >= 2) break;
    }
    // Missing page text is not silently treated as complete extraction; OCR is a separate capability.
    // 无文本页面不能冒充完整提取；OCR 属于独立能力，首阶段明确拒绝不完整的页面证据。
    if (missingTextPages.length) throw Object.assign(documentExtractionFailure('OCR_UNAVAILABLE'), { pages: missingTextPages });
    const lastPage = pages.at(-1).page;
    return { text, pageCount, pages,
      pageWindow: { version: 'pdf-page-window-v1', startPage, endPage: lastPage, nextPage: lastPage < pageCount ? lastPage + 1 : null },
      complete: startPage === 1 && lastPage === pageCount,
      uncoveredRanges: [...(startPage > 1 ? [{ startPage: 1, endPage: startPage - 1 }] : []),
        ...(lastPage < pageCount ? [{ startPage: lastPage + 1, endPage: pageCount }] : [])], ...(ocr ? { ocr } : {}) };
  } catch (error) {
    if (error.name === 'PasswordException') throw documentExtractionFailure('DOCUMENT_ENCRYPTED');
    error.details = { ...error.details, ...(pageCount === undefined ? {} : { pageCount }), processedPages,
      ...(requestedWindow ? { pageWindow: requestedWindow } : {}) };
    throw error;
  } finally { await task.destroy(); }
}

async function inspectDocxArchive(bytes, limits) {
  requireVersion('yauzl', '3.4.0');
  const archive = await require('yauzl').fromBufferPromise(bytes,
    { lazyEntries: true, autoClose: false, validateEntrySizes: true, strictFileNames: true });
  let expandedBytes = 0, declaredBytes = 0, entries = 0, hasDocument = false, hasContentTypes = false;
  const names = new Set();
  try {
    for await (const entry of archive.eachEntry()) {
      if (++entries > limits.maximumArchiveEntries) throw documentExtractionFailure('DOCUMENT_ARCHIVE_LIMIT',
        { limit: { dimension: 'maximumArchiveEntries', limit: limits.maximumArchiveEntries, observed: entries } });
      if (entry.uncompressedSize > limits.maximumExpandedBytes) throw documentExtractionFailure('DOCUMENT_ARCHIVE_LIMIT',
        { limit: { dimension: 'maximumExpandedBytes', limit: limits.maximumExpandedBytes, observed: entry.uncompressedSize } });
      declaredBytes += entry.uncompressedSize;
      if (declaredBytes > limits.maximumExpandedBytes) throw documentExtractionFailure('DOCUMENT_ARCHIVE_LIMIT',
        { limit: { dimension: 'maximumExpandedBytes', limit: limits.maximumExpandedBytes, observed: declaredBytes } });
      if (names.has(entry.fileName)) throw documentExtractionFailure('DOCUMENT_PARSE_FAILED');
      names.add(entry.fileName);
      if (entry.generalPurposeBitFlag & 1) throw documentExtractionFailure('DOCUMENT_ENCRYPTED');
      if (entry.fileName.endsWith('/')) continue;
      const stream = await archive.openReadStreamPromise(entry);
      const xml = /\.(?:xml|rels)$/iu.test(entry.fileName), chunks = [];
      try {
        for await (const chunk of stream) {
          expandedBytes += chunk.length;
          if (expandedBytes > limits.maximumExpandedBytes) throw documentExtractionFailure('DOCUMENT_ARCHIVE_LIMIT',
            { limit: { dimension: 'maximumExpandedBytes', limit: limits.maximumExpandedBytes, observed: expandedBytes } });
          if (xml) chunks.push(chunk);
        }
      } finally { stream.destroy(); }
      if (xml) {
        const content = new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(chunks));
        // Validate expanded XML before the document library builds its tree; DTD/entity expansion is unnecessary.
        // 文档库建树前核验已展开 XML；普通 DOCX 不需要 DTD/实体声明，拒绝隐式实体展开。
        if (/<!\s*(?:DOCTYPE|ENTITY)\b/iu.test(content)) throw documentExtractionFailure('DOCUMENT_XML_UNSUPPORTED');
        if (entry.fileName === 'word/document.xml') hasDocument = true;
        if (entry.fileName === '[Content_Types].xml')
          hasContentTypes = content.includes('application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml');
      }
      observeMemory(limits);
    }
    if (!hasDocument || !hasContentTypes) throw documentExtractionFailure('DOCUMENT_PARSE_FAILED');
  } finally { archive.close(); }
}

async function docxText(bytes, limits) {
  requireVersion('mammoth', '1.13.0');
  await inspectDocxArchive(bytes, limits);
  const result = await require('mammoth').extractRawText({ buffer: bytes });
  if (result.messages?.length) throw documentExtractionFailure('DOCUMENT_PARSE_FAILED');
  if (Buffer.byteLength(result.value) > limits.maximumOutputBytes) throw documentExtractionFailure('DOCUMENT_OUTPUT_LIMIT',
    { limit: { dimension: 'maximumOutputBytes', limit: limits.maximumOutputBytes, observed: Buffer.byteLength(result.value) } });
  if (!result.value.trim()) throw documentExtractionFailure('DOCUMENT_TEXT_UNAVAILABLE');
  observeMemory(limits);
  return { text: result.value };
}

/** A disposable decoder receives authorized bytes, never paths, URLs, scripts or rendering requests.
 * 一次性解码器只接收已授权字节，不接收路径、URL、脚本或渲染请求；父进程断开立即退役。
 */
process.on('disconnect', () => process.exit(1));
process.on('message', async message => {
  if (message?.type === 'ocr-result') {
    const pending = ocrReplies.get(message.requestId);
    if (!pending) return;
    ocrReplies.delete(message.requestId);
    if (message.errorCode) pending.reject(documentExtractionFailure(message.errorCode));
    else pending.resolve(message);
    return;
  }
  if (started) return;
  started = true;
  let memoryTimer;
  try {
    const limits = validateDocumentExtractionLimits(message?.limits);
    if (message?.type !== 'decode' || !['pdf', 'docx'].includes(message.format) || !(message.bytes instanceof Uint8Array))
      throw documentExtractionFailure('DOCUMENT_INVALID_INPUT');
    const bytes = Buffer.from(message.bytes);
    if (!bytes.length || bytes.length > limits.maximumInputBytes) throw documentExtractionFailure('DOCUMENT_BYTES_LIMIT');
    memoryTimer = setInterval(() => {
      try { observeMemory(limits); }
      catch { process.send?.({ type: 'error', code: 'DOCUMENT_DECODER_MEMORY_LIMIT' }); }
    }, 25);
    const result = await (message.format === 'pdf' ? pdfText(bytes, limits,
      { pageWindow: message.pageWindow, ocrEnabled: message.ocrEnabled === true }) : docxText(bytes, limits));
    observeMemory(limits);
    process.send?.({ type: 'result', ...result });
  } catch (error) {
    const code = /^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$/u.test(error?.code ?? '') ? error.code : 'DOCUMENT_PARSE_FAILED';
    process.send?.({ type: 'error', code, ...(error.pages ? { pages: error.pages } : {}), ...(error.details ? { details: error.details } : {}) });
  } finally { clearInterval(memoryTimer); }
});
