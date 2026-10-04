import { compile } from 'html-to-text';
import { boundedInteger, toolFailure } from './tool-paths.mjs';
import { fetchPublicWebPage } from './web-http-transport.mjs';

const maximumExtractedCharacters = 262144;
const ellipsis = '\n[KYNXA_WEB_EXTRACTION_LIMIT]\n';
const limits = { maxInputLength: 2097152, maxDepth: 64, maxChildNodes: 20000, ellipsis };
const titleText = compile({ wordwrap: false, limits, baseElements: { selectors: ['title'], returnDomByDefault: false } });
const pageText = compile({
  wordwrap: false, limits,
  selectors: [
    ...['script', 'style', 'noscript', 'template', 'svg', 'img', 'input', 'textarea', '[hidden]', '[aria-hidden="true"]']
      .map(selector => ({ selector, format: 'skip' })),
    { selector: 'a', options: { hideLinkHrefIfSameAsText: true,
      pathRewrite: (href, metadata) => {
        try {
          const url = new URL(href, metadata.url);
          return ['http:', 'https:'].includes(url.protocol) && !url.username && !url.password ? url.href : '';
        } catch { return ''; }
      } } }
  ]
});

export const webFetchDescriptor = {
  name: 'web.fetch', source: 'builtin',
  description: 'Read a public HTTP(S) page as text, preserving its final URL and links. No Python, cookies or JavaScript. Private/local, signed-in and interactive pages need browser MCP. Full bounded extracted text is archived; use tool.result.read for stable paging. Each fetch is live.',
  inputSchema: { type: 'object', additionalProperties: false,
    properties: { url: { type: 'string', minLength: 1, maxLength: 8192 },
      offset: { type: 'integer', minimum: 0, maximum: maximumExtractedCharacters },
      limit: { type: 'integer', minimum: 1, maximum: 16000 },
      timeoutMs: { type: 'integer', minimum: 100, maximum: 60000 },
      reason: { type: 'string', minLength: 1, maxLength: 2000 } },
    required: ['url', 'reason'] }
};

function decodePage(bytes, contentType) {
  const label = /charset\s*=\s*["']?([^\s;"']+)/iu.exec(contentType)?.[1] ?? 'utf-8';
  let decoder;
  try { decoder = new TextDecoder(label, { fatal: true }); }
  catch { throw toolFailure('网页字符编码不受支持，请使用浏览器读取。', 'WEB_UNSUPPORTED_ENCODING', 415); }
  try { return decoder.decode(bytes); }
  catch { throw toolFailure('网页文本编码无效，请使用浏览器读取。', 'WEB_INVALID_ENCODING', 415); }
}

/** Text conversion reuses the pinned upstream parser. Fetch never executes page scripts or imports a browser profile. */
export class WebFetchTool {
  constructor({ fetchPage = fetchPublicWebPage } = {}) { this.fetchPage = fetchPage; this.active = new Set(); this.closed = false; }

  async run(input, signal) {
    if (this.closed) throw toolFailure('网页读取服务已关闭。', 'TOOL_SERVICE_CLOSED', 409);
    const controller = new AbortController();
    this.active.add(controller);
    try { return await this._run(input, signal ? AbortSignal.any([signal, controller.signal]) : controller.signal); }
    finally { this.active.delete(controller); }
  }

  close() {
    this.closed = true;
    for (const controller of this.active) controller.abort();
  }

  async _run(input, signal) {
    signal?.throwIfAborted();
    const timeoutMs = boundedInteger(input.timeoutMs, 20000, 100, 60000);
    const page = await this.fetchPage(input.url, { signal, timeoutMs });
    signal?.throwIfAborted();
    const mediaType = page.headers.contentType.split(';')[0].trim().toLowerCase();
    const decoded = decodePage(page.bytes, page.headers.contentType);
    if (decoded.includes('\0')) throw toolFailure('网页不是受支持的文本。', 'WEB_UNSUPPORTED_CONTENT', 415);
    const html = ['text/html', 'application/xhtml+xml'].includes(mediaType);
    const extracted = html ? pageText(decoded, { url: page.url }) : decoded;
    const title = html ? titleText(decoded).trim().slice(0, 512) : '';
    const bounded = extracted.slice(0, maximumExtractedCharacters);
    const full = /[\uD800-\uDBFF]$/u.test(bounded) ? bounded.slice(0, -1) : bounded;
    const extractionTruncated = full.length < extracted.length || extracted.includes(ellipsis.trim());
    const offset = boundedInteger(input.offset, 0, 0, maximumExtractedCharacters);
    const limit = boundedInteger(input.limit, 16000, 1, 16000);
    if (offset > full.length || (offset > 0 && /[\uDC00-\uDFFF]/u.test(full[offset] ?? '') && /[\uD800-\uDBFF]/u.test(full[offset - 1])))
      throw toolFailure('网页分页位置无效，请使用返回的 nextOffset。', 'WEB_INVALID_OFFSET');
    let end = Math.min(full.length, offset + limit);
    if (end < full.length && /[\uD800-\uDBFF]/u.test(full[end - 1] ?? '')) end--;
    if (end === offset && end < full.length) end = Math.min(full.length, end + 2);
    const metadata = { requestedUrl: input.url, url: page.url, status: page.status, mediaType, title,
      responseBytes: page.bytes.length, redirects: page.redirects, extractedCharacters: full.length,
      extractionTruncated, fetchedAt: new Date().toISOString(), javascriptExecuted: false };
    const value = { ...metadata, content: full.slice(offset, end), offset, nextOffset: end,
      hasMore: end < full.length, truncated: offset > 0 || end < full.length || extractionTruncated };
    // Keep the complete bounded source separate from the short model page. Existing result references own retrieval and storage.
    const canonical = { content: [{ type: 'resource_link', uri: page.url, name: title || page.url, mimeType: 'text/plain' }],
      structuredContent: { ...metadata, content: full }, isError: false };
    return { value, canonical, content: JSON.stringify(value), isError: false, outsideWorkspace: true };
  }
}
