import { lookup as lookupAddress } from 'node:dns/promises';
import { request as requestHttp } from 'node:http';
import { request as requestHttps } from 'node:https';
import { isIP } from 'node:net';
import { Transform, Writable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { createBrotliDecompress, createGunzip, createInflate } from 'node:zlib';
import { toolFailure } from '../platform/tool-paths.mjs';

const MAX_RESPONSE_BYTES = 2 * 1024 * 1024;
const MAX_REDIRECTS = 5;
const REDIRECT_STATUSES = new Set([301, 302, 303, 307, 308]);
const blockedUrl = () => toolFailure('网页工具仅支持公共 HTTP(S) 地址，不能访问本机、内网或带凭据的地址。', 'WEB_URL_BLOCKED', 403);
const tooLarge = () => toolFailure('网页响应超过 2 MiB 限制。', 'WEB_RESPONSE_TOO_LARGE', 413);
const unsupportedContent = () => toolFailure('网页响应类型或压缩方式不受支持。', 'WEB_UNSUPPORTED_CONTENT', 415);
const cancelled = () => Object.assign(new Error('网页获取已取消。'), { name: 'AbortError', code: 'ABORT_ERR' });

function ipv4ToNumber(address) {
  return address.split('.').reduce((result, part) => (result * 256) + Number(part), 0);
}

const nonPublicIpv4Ranges = [
  ['0.0.0.0', 8], ['10.0.0.0', 8], ['100.64.0.0', 10], ['127.0.0.0', 8],
  ['169.254.0.0', 16], ['172.16.0.0', 12], ['192.0.0.0', 24], ['192.0.2.0', 24],
  ['192.88.99.0', 24], ['192.168.0.0', 16], ['198.18.0.0', 15], ['198.51.100.0', 24],
  ['203.0.113.0', 24], ['224.0.0.0', 4], ['240.0.0.0', 4]
].map(([address, prefixLength]) => ({ start: ipv4ToNumber(address), size: 2 ** (32 - prefixLength) }));

function ipv6ToBigInt(address) {
  const [left, right = ''] = address.split('::');
  const first = left ? left.split(':') : [];
  const last = right ? right.split(':') : [];
  const parts = address.includes('::') ? [...first, ...Array(8 - first.length - last.length).fill('0'), ...last] : first;
  return parts.reduce((result, part) => (result << 16n) + BigInt('0x' + part), 0n);
}

function isWithinIpv6Prefix(address, base, prefixLength) {
  const shift = 128n - BigInt(prefixLength);
  return (address >> shift) === (ipv6ToBigInt(base) >> shift);
}

function isPublicAddress(address) {
  const family = isIP(address);
  if (family === 4) {
    const value = ipv4ToNumber(address);
    return !nonPublicIpv4Ranges.some(range => value >= range.start && value < range.start + range.size);
  }
  if (family !== 6 || address.includes('.')) return false;
  const value = ipv6ToBigInt(address);
  // Global unicast only, excluding IETF special uses, documentation and IPv4 tunnels.
  // 只允许全球单播地址，排除 IETF 特殊用途、文档地址和 IPv4 隧道。
  return isWithinIpv6Prefix(value, '2000::', 3) && ![
    ['2001::', 23], ['2001:db8::', 32], ['2002::', 16], ['3fff::', 20]
  ].some(([base, prefixLength]) => isWithinIpv6Prefix(value, base, prefixLength));
}

/**
 * Pure validation used before approval; DNS and pinned connections remain execution-time checks.
 * 审批前只做纯参数校验，DNS 和固定连接地址仍在执行时检查。
 */
export function validatePublicWebUrl(value) {
  if (typeof value !== 'string' || !value || value.length > 8192 ||
      /[\u0000-\u0020\u007f\\]/u.test(value) || /%(?:0[0-9a-f]|1[0-9a-f]|7f)/i.test(value) ||
      !/^https?:\/\//i.test(value)) throw blockedUrl();
  let url;
  try { url = new URL(value); } catch { throw blockedUrl(); }
  const authority = value.slice(value.indexOf('://') + 3).split(/[/?#]/u)[0];
  const hostname = url.hostname.replace(/^\[|\]$/g, '').replace(/\.$/, '');
  if (!hostname || authority.includes('@') || url.username || url.password ||
      (url.port && !['80', '443'].includes(url.port)) ||
      /(?:^|\.)(?:localhost|local|internal|home|lan|onion|arpa)$/i.test(hostname) ||
      (isIP(hostname) && !isPublicAddress(hostname))) throw blockedUrl();
  url.hash = '';
  return url;
}

function throwIfAborted(signal) {
  if (signal.aborted) throw signal.reason;
}

async function abortable(operation, signal) {
  throwIfAborted(signal);
  return new Promise((resolve, reject) => {
    let settled = false;
    const finish = (complete, value) => {
      if (settled) return;
      settled = true;
      signal.removeEventListener('abort', stopped);
      complete(value);
    };
    const stopped = () => finish(reject, signal.reason);
    signal.addEventListener('abort', stopped, { once: true });
    Promise.resolve(operation).then(value => finish(resolve, value), error => finish(reject, error));
    if (signal.aborted) stopped();
  });
}

async function validatedAddresses(url, lookup, signal) {
  throwIfAborted(signal);
  const hostname = url.hostname.replace(/^\[|\]$/g, '');
  const family = isIP(hostname);
  if (family) return [{ address: hostname, family }];
  let addresses;
  try { addresses = await abortable(lookup(hostname, { all: true, verbatim: true }), signal); }
  catch (error) {
    if (signal.aborted) throw signal.reason;
    throw toolFailure('公共网页地址解析失败。', 'WEB_HTTP_ERROR', 502);
  }
  if (!Array.isArray(addresses) || !addresses.length || addresses.length > 64 ||
      addresses.some(item => !item || !isPublicAddress(item.address) || item.family !== isIP(item.address))) {
    // A fake-IP hint is not a private-network exception; all direct HTTP sockets remain blocked.
    // 虚拟 IP 提示不是私网例外；原始 HTTP 连接仍全部拒绝，不为普通私网或无效 DNS 建议绕过。
    const isProxyRange = item => item?.family === 4 && isIP(item.address) === 4 && /^198\.(?:18|19)\./u.test(item.address);
    const allowBackgroundReadHint = Array.isArray(addresses) && addresses.length > 0 && addresses.length <= 64 &&
      addresses.some(isProxyRange) && addresses.every(item => item && item.family === isIP(item.address) &&
        (isPublicAddress(item.address) || isProxyRange(item)));
    throw Object.assign(toolFailure('当前 DNS 答案包含无效或非公共地址，可能来自代理的虚拟 IP；本次 HTTP 读取已拒绝，不会自动打开本机浏览器。', 'WEB_URL_BLOCKED', 403),
      { webFailureReason: 'dns-non-public', allowBackgroundReadHint });
  }
  return addresses.map(item => ({ address: item.address, family: item.family }));
}

function pinnedLookup(addresses) {
  return (_hostname, options, callback) => {
    if (typeof options === 'function') { callback = options; options = {}; }
    const family = typeof options === 'number' ? options : options?.family;
    const choices = family ? addresses.filter(item => item.family === family) : addresses;
    if (!choices.length) return queueMicrotask(() => callback(blockedUrl()));
    if (options?.all) return queueMicrotask(() => callback(null, choices.map(item => ({ ...item }))));
    queueMicrotask(() => callback(null, choices[0].address, choices[0].family));
  };
}

function requestPage(url, addresses, request, signal) {
  throwIfAborted(signal);
  return new Promise((resolve, reject) => {
    let outgoing;
    try {
      outgoing = request(url, {
        method: 'GET', agent: false, signal, lookup: pinnedLookup(addresses),
        headers: { Accept: 'text/html, application/xhtml+xml, text/plain, application/json, application/xml;q=0.9, text/*;q=0.8',
          'Accept-Encoding': 'gzip, deflate, br', 'User-Agent': 'KYNXA/0.1 PublicWebFetch' }
      }, response => resolve({ request: outgoing, response }));
      outgoing.once('error', () => reject(signal.aborted ? signal.reason :
        toolFailure('公共网页连接未完成。', 'WEB_HTTP_ERROR', 502)));
      outgoing.end();
    } catch {
      outgoing?.destroy();
      reject(signal.aborted ? signal.reason : toolFailure('公共网页连接未完成。', 'WEB_HTTP_ERROR', 502));
    }
  });
}

function contentHeaders(response) {
  const contentType = response.headers['content-type'];
  const contentEncoding = response.headers['content-encoding'] ?? 'identity';
  if (typeof contentType !== 'string' || contentType.length > 1024 || typeof contentEncoding !== 'string') throw unsupportedContent();
  const mediaType = contentType.split(';')[0].trim().toLowerCase();
  if (!/^text\/[a-z0-9!#$&^_.+-]+$/i.test(mediaType) &&
      !/^application\/(?:json|xml|[a-z0-9!#$&^_.+-]+\+(?:json|xml))$/i.test(mediaType)) throw unsupportedContent();
  const encoding = contentEncoding.trim().toLowerCase();
  if (!['identity', 'gzip', 'deflate', 'br'].includes(encoding)) throw unsupportedContent();
  const length = response.headers['content-length'];
  if (typeof length === 'string' && /^\d+$/.test(length) && Number(length) > MAX_RESPONSE_BYTES) throw tooLarge();
  return { contentType, contentEncoding: encoding };
}

function byteLimit() {
  let receivedBytes = 0;
  return new Transform({ transform(chunk, _encoding, next) {
    receivedBytes += chunk.length;
    if (receivedBytes > MAX_RESPONSE_BYTES) return next(tooLarge());
    next(null, chunk);
  } });
}

async function readPage(response, headers, signal) {
  const chunks = [];
  const stages = [response, byteLimit()];
  if (headers.contentEncoding === 'gzip') stages.push(createGunzip());
  if (headers.contentEncoding === 'deflate') stages.push(createInflate());
  if (headers.contentEncoding === 'br') stages.push(createBrotliDecompress());
  stages.push(byteLimit(), new Writable({ write(chunk, _encoding, next) { chunks.push(chunk); next(); } }));
  try { await pipeline(stages, { signal }); }
  catch (error) {
    if (signal.aborted) throw signal.reason;
    if (error.code === 'WEB_RESPONSE_TOO_LARGE') throw error;
    throw toolFailure('公共网页响应未完整读取。', 'WEB_HTTP_ERROR', 502);
  }
  return Buffer.concat(chunks);
}

/**
 * Public GET only: resolve and validate every address, then pin the socket lookup for each redirect.
 * 只发公开 GET，每次重定向先解析并验证全部地址，再固定套接字查询结果。
 */
export async function fetchPublicWebPage(value, { signal, timeoutMs = 20000 } = {}, dependencies = {}) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 120000)
    throw toolFailure('网页超时须在 1–120000 毫秒之间。');
  const controller = new AbortController();
  const stop = () => controller.abort(cancelled());
  signal?.addEventListener('abort', stop, { once: true });
  if (signal?.aborted) stop();
  const timeoutTimer = setTimeout(() => controller.abort(toolFailure('公共网页获取超时。', 'WEB_TIMEOUT', 504)), timeoutMs);
  timeoutTimer.unref();
  const lookup = dependencies.lookup ?? lookupAddress;
  const httpRequest = dependencies.httpRequest ?? requestHttp;
  const httpsRequest = dependencies.httpsRequest ?? requestHttps;
  const redirects = [];
  let active;
  try {
    let url = validatePublicWebUrl(value);
    let addresses = await validatedAddresses(url, lookup, controller.signal);
    for (;;) {
      throwIfAborted(controller.signal);
      active = await requestPage(url, addresses, url.protocol === 'https:' ? httpsRequest : httpRequest, controller.signal);
      const { response } = active, status = response.statusCode ?? 0;
      if (REDIRECT_STATUSES.has(status)) {
        if (redirects.length >= MAX_REDIRECTS || typeof response.headers.location !== 'string')
          throw Object.assign(toolFailure('公共网页重定向未完成。', 'WEB_HTTP_ERROR', 502), { status, httpStatus: status });
        if (/[\u0000-\u0020\u007f\\]/u.test(response.headers.location)) throw blockedUrl();
        let target;
        try { target = new URL(response.headers.location, url).href; } catch { throw blockedUrl(); }
        const next = validatePublicWebUrl(target);
        // The next DNS lookup is checked before a connection is attempted. No cookies carry across hops.
        // 尝试连接前检查下一跳 DNS，不在不同跳之间携带 Cookie。
        const nextAddresses = await validatedAddresses(next, lookup, controller.signal);
        redirects.push(next.href);
        response.destroy(); active.request.destroy(); active = undefined;
        url = next; addresses = nextAddresses;
        continue;
      }
      if (status < 200 || status >= 300)
        throw Object.assign(toolFailure(`公共网页返回 HTTP ${status}。`, 'WEB_HTTP_ERROR', 502), { status, httpStatus: status });
      const headers = contentHeaders(response);
      const bytes = await readPage(response, headers, controller.signal);
      return { url: url.href, status, headers, bytes, redirects };
    }
  } finally {
    active?.response.destroy(); active?.request.destroy();
    clearTimeout(timeoutTimer);
    signal?.removeEventListener('abort', stop);
  }
}
