import { fork } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateSourceExtraction } from '../../data/retrieval/retrieval-contracts.mjs';
import { validateDocumentCoverage } from '../../data/retrieval/document-coverage.mjs';
import { DOCUMENT_EXTRACTION_LIMITS, DOCUMENT_EXTRACTION_VERSIONS, documentExtractionFailure,
  validateDocumentExtractionLimits, validateDocumentPageWindow } from './document-extraction-contracts.mjs';
import { recognizePdfPages } from './windows-document-ocr.mjs';

const DECODER_ROOT = dirname(fileURLToPath(import.meta.url));
const GATEWAY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
let activeProcesses = 0;

const abortFailure = () => Object.assign(new Error('Document extraction cancelled. / 文档提取已取消。'),
  { name: 'AbortError', code: 'ABORT_ERR' });

export function documentExtractionCoverage(format, limits, code, rawContentHash, details = {}) {
  const pageCount = Number.isSafeInteger(details.pageCount) && details.pageCount > 0 ? details.pageCount : undefined;
  const processedPages = pageCount !== undefined && Number.isSafeInteger(details.processedPages) && details.processedPages >= 0
    ? Math.min(details.processedPages, pageCount) : 0;
  const limit = details.limit;
  const window = details.pageWindow;
  const startPage = window?.startPage ?? 1;
  return validateDocumentCoverage({ format, reasonCode: code, complete: false, processedPages, publishedPages: 0,
    ...(rawContentHash ? { rawContentHash } : {}), ...(pageCount === undefined ? {} : { pageCount }),
    uncoveredRanges: pageCount === undefined ? [] : [{ startPage: 1, endPage: pageCount }],
    unprocessedRanges: pageCount === undefined ? [] : [...(startPage > 1 ? [{ startPage: 1, endPage: startPage - 1 }] : []),
      ...(startPage + processedPages <= pageCount ? [{ startPage: startPage + processedPages, endPage: pageCount }] : [])],
    ...(Array.isArray(details.pages) ? { missingTextPages: details.pages.filter(page => Number.isSafeInteger(page) &&
      page >= 1 && page <= pageCount).slice(0, 100) } : {}),
    effectiveLimits: { maximumInputBytes: limits.maximumInputBytes, maximumOutputBytes: limits.maximumOutputBytes, maximumPages: limits.maximumPages },
    ...(limit ? { limit } : {}), readback: { kind: 'original-document',
      ...(pageCount === undefined ? {} : { startPage, endPage: window?.endPage ?? pageCount }) } });
}

/** Decode only bytes supplied after path authorization; every invocation owns and retires its process.
 * 仅解码路径授权后提供的字节；每次调用独占并回收子进程，原生退出或解压 OOM 不能终止网关。 */
export async function extractDocumentBytes(bytes, format, { signal, forkFactory = fork, resourceService,
  pageWindow, ocrBackend = process.platform === 'win32' ? recognizePdfPages : null, ...options } = {}) {
  signal?.throwIfAborted();
  const limits = validateDocumentExtractionLimits(options);
  if (!(bytes instanceof Uint8Array) || !DOCUMENT_EXTRACTION_VERSIONS[format])
    throw documentExtractionFailure('DOCUMENT_INVALID_INPUT');
  const rawContentHash = bytes.length <= limits.maximumInputBytes ? createHash('sha256').update(bytes).digest('hex') : undefined;
  pageWindow = validateDocumentPageWindow(pageWindow, limits.maximumPages);
  if (pageWindow && format !== 'pdf') throw documentExtractionFailure('DOCUMENT_INVALID_PAGE_WINDOW');
  if (pageWindow?.rawContentHash && pageWindow.rawContentHash !== rawContentHash)
    throw documentExtractionFailure('DOCUMENT_SOURCE_CHANGED');
  if (!bytes.length || bytes.length > limits.maximumInputBytes) throw documentExtractionFailure('DOCUMENT_BYTES_LIMIT', {
    documentCoverage: documentExtractionCoverage(format, limits, 'DOCUMENT_BYTES_LIMIT', rawContentHash,
      { limit: { dimension: 'maximumInputBytes', limit: limits.maximumInputBytes, observed: bytes.length } }) });
  if (activeProcesses >= DOCUMENT_EXTRACTION_LIMITS.maximumProcesses) throw documentExtractionFailure('DOCUMENT_DECODER_BUSY', {
    documentCoverage: documentExtractionCoverage(format, limits, 'DOCUMENT_DECODER_BUSY', rawContentHash) });
  activeProcesses++;
  let child, timeout, renewal, result, rejection, lease, resourceFailure;
  const ocrCancellation = new AbortController(), ocrTasks = new Set(), ocrPages = new Set();
  let terminated = false, closed = false, slotReleased = false, resourcesReleased = false;
  const releaseSlot = () => { if (!slotReleased) { activeProcesses--; slotReleased = true; } };
  const releaseResources = async () => {
    if (!lease || resourcesReleased) return;
    resourcesReleased = true; clearInterval(renewal);
    try { await resourceService.release(lease.leaseId); }
    catch { resourceFailure = documentExtractionFailure('DOCUMENT_RESOURCE_FAILED'); }
  };
  try {
    if (resourceService) {
      lease = await resourceService.acquire({ taskId: `document:${randomUUID()}`, kind: 'background', cpuThreads: 1,
        memoryBytes: limits.maximumRssBytes + bytes.byteLength * 2 + limits.maximumOutputBytes * 2,
        waitMs: Math.min(limits.timeoutMs, 10000), ttlMs: 30000 }, { signal });
      if (lease.status !== 'granted') { lease = null; throw documentExtractionFailure('DOCUMENT_RESOURCE_BUSY'); }
      signal?.throwIfAborted();
      renewal = setInterval(() => resourceService.renew(lease.leaseId, { ttlMs: 30000 }).catch(() => {}), 10000);
      renewal.unref?.();
    }
  } catch (error) {
    releaseSlot(); await releaseResources();
    if (error.name !== 'AbortError' && /^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$/u.test(error.code ?? ''))
      error.details = { ...error.details, documentCoverage: documentExtractionCoverage(format, limits, error.code, rawContentHash) };
    throw error;
  }
  let resolveClose;
  const exit = new Promise(resolveExit => { resolveClose = resolveExit; });
  const stop = () => {
    if (!child || terminated) return;
    terminated = true;
    ocrCancellation.abort();
    try { child.kill('SIGKILL'); } catch { /* Exit observation retains the occupied slot. / 未观察到退出时保留并发槽。 */ }
  };
  let rejectResult;
  const outcome = new Promise((resolveResult, reject) => {
    rejectResult = reject;
    const fail = error => { rejection ??= error; stop(); reject(rejection); };
    try {
      child = forkFactory(new URL('./document-extraction-worker.mjs', import.meta.url), [], {
        windowsHide: true, serialization: 'advanced', stdio: ['ignore', 'ignore', 'ignore', 'ipc'],
        execArgv: [`--max-old-space-size=${limits.maximumHeapMb}`, '--permission', '--allow-addons',
          `--allow-fs-read=${DECODER_ROOT}`, `--allow-fs-read=${join(GATEWAY_ROOT, 'node_modules')}`],
        env: { SystemRoot: process.env.SystemRoot, WINDIR: process.env.WINDIR, PATH: process.env.PATH, NODE_NO_WARNINGS: '1' }
      });
    } catch {
      closed = true; releaseSlot(); releaseResources().finally(resolveClose);
      fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); return;
    }
    child.once('close', () => { closed = true; releaseSlot(); releaseResources().finally(resolveClose); });
    child.on('error', () => fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')));
    child.on('message', message => {
      if (terminated || rejection || !message || typeof message !== 'object') return;
      if (message.type === 'memory') {
        if (!Number.isSafeInteger(message.rssBytes) || message.rssBytes > limits.maximumRssBytes)
          fail(documentExtractionFailure('DOCUMENT_DECODER_MEMORY_LIMIT'));
        return;
      }
      if (message.type === 'ocr-request') {
        if (format !== 'pdf' || typeof ocrBackend !== 'function' || ocrTasks.size || ocrPages.has(message.requestId) ||
            !Number.isSafeInteger(message.requestId) ||
            !Array.isArray(message.pages) || message.pages.length !== 1 || message.pages[0] !== message.requestId ||
            message.pages[0] < (pageWindow?.startPage ?? 1) ||
            message.pages[0] > (pageWindow?.endPage ?? (pageWindow?.startPage ?? 1) + limits.maximumPages - 1)) {
          fail(documentExtractionFailure('OCR_INVALID_REQUEST')); return;
        }
        ocrPages.add(message.requestId);
        const ocrSignal = signal ? AbortSignal.any([signal, ocrCancellation.signal]) : ocrCancellation.signal;
        const operation = Promise.resolve().then(() => ocrBackend(bytes, message.pages, { signal: ocrSignal, resourceService,
          maximumOutputBytes: limits.maximumOutputBytes, timeoutMs: Math.min(8000, limits.timeoutMs) }))
          .then(receipt => {
            if (terminated || rejection) return;
            const page = receipt.pages?.find(item => item.page === message.requestId);
            if (!page || typeof page.text !== 'string' || Buffer.byteLength(page.text) > limits.maximumOutputBytes ||
                typeof receipt.version !== 'string' || typeof receipt.language !== 'string')
              throw documentExtractionFailure('OCR_TEXT_UNAVAILABLE');
            child.send({ type: 'ocr-result', requestId: message.requestId, text: page.text, version: receipt.version, language: receipt.language });
          }).catch(error => {
            if (terminated || rejection) return;
            child.send({ type: 'ocr-result', requestId: message.requestId,
              errorCode: /^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$/u.test(error.code ?? '') ? error.code : 'OCR_FAILED' });
          }).finally(() => ocrTasks.delete(operation));
        ocrTasks.add(operation);
        return;
      }
      if (message.type === 'error') {
        const code = typeof message.code === 'string' && /^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$/u.test(message.code)
          ? message.code : 'DOCUMENT_PARSE_FAILED';
        const pages = Array.isArray(message.pages) ? message.pages.filter(page => Number.isInteger(page) && page >= 1 &&
          page <= (message.details?.pageCount ?? 1000000)).slice(0, limits.maximumPages) : null;
        try {
          fail(documentExtractionFailure(code, { ...(pages ? { pages } : {}),
            documentCoverage: documentExtractionCoverage(format, limits, code, rawContentHash, { ...message.details, ...(pages ? { pages } : {}) }) }));
        } catch { fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); }
        return;
      }
      if (message.type !== 'result') { fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); return; }
      try {
        if (typeof message.text !== 'string' || !message.text.trim() || message.text.includes('\0') ||
            Buffer.byteLength(message.text) > limits.maximumOutputBytes)
          throw documentExtractionFailure('DOCUMENT_OUTPUT_LIMIT');
        const extraction = validateSourceExtraction({ format, version: DOCUMENT_EXTRACTION_VERSIONS[format],
          rawContentHash,
          ...(format === 'pdf' ? { pageCount: message.pageCount, pages: message.pages,
            ...(message.pageWindow ? { pageWindow: message.pageWindow, complete: message.complete, uncoveredRanges: message.uncoveredRanges } : {}),
            ...(message.ocr ? { ocr: message.ocr } : {}) } : {}) }, message.text.length);
        result = { text: message.text, extraction };
        stop();
        resolveResult(result);
      } catch (error) { fail(error); }
    });
    child.on('exit', () => { if (!result && !rejection) fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); });
    timeout = setTimeout(() => fail(documentExtractionFailure('DOCUMENT_DECODER_TIMEOUT')), limits.timeoutMs);
    const send = () => {
      if (terminated || rejection) return;
      try { child.send({ type: 'decode', format, bytes, limits,
        ...(format === 'pdf' ? { ...(pageWindow ? { pageWindow } : {}), ocrEnabled: typeof ocrBackend === 'function' } : {}) },
        error => { if (error) fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); }); }
      catch { fail(documentExtractionFailure('DOCUMENT_DECODER_FAILED')); }
    };
    if (resourceService) {
      // Register only this owned process, and do so before sending expensive decode work.
      // 只登记本次自有解码进程，在发送高开销提取工作前完成进程登记。
      Promise.resolve().then(() => resourceService.registerExecutor(lease.leaseId, { processId: child.pid }))
        .then(receipt => { if (receipt?.status !== 'registered') fail(documentExtractionFailure('DOCUMENT_RESOURCE_FAILED')); else send(); })
        .catch(() => fail(documentExtractionFailure('DOCUMENT_RESOURCE_FAILED')));
    } else send();
  });
  const abort = () => { rejection ??= abortFailure(); stop(); rejectResult(rejection); };
  signal?.addEventListener('abort', abort, { once: true });
  if (signal?.aborted) abort();
  let decoded, failure;
  try { decoded = await outcome; }
  catch (error) { failure = error; }
  finally {
    clearTimeout(timeout);
    stop();
    await Promise.allSettled([...ocrTasks]);
    // A missing close cannot hang a job or falsely free a live decoder's concurrency slot.
    // 缺少退出事件不能无限挂住任务，也不能把仍存活的解码进程误记为已回收。
    let closeTimeout;
    await Promise.race([exit, new Promise(resolveWait => { closeTimeout = setTimeout(resolveWait, 2000); })]);
    clearTimeout(closeTimeout);
    signal?.removeEventListener('abort', abort);
    if (!closed) failure ??= documentExtractionFailure('DOCUMENT_DECODER_FAILED');
  }
  if (failure) {
    if (failure.name !== 'AbortError' && /^(?:DOCUMENT_[A-Z_]+|OCR_[A-Z_]+)$/u.test(failure.code ?? '') && !failure.details?.documentCoverage)
      failure.details = { ...failure.details, documentCoverage: documentExtractionCoverage(format, limits, failure.code, rawContentHash) };
    throw failure;
  }
  if (resourceFailure) throw resourceFailure;
  signal?.throwIfAborted();
  return decoded;
}
