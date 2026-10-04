import assert from 'node:assert/strict';
import { createServer, request as requestHttp } from 'node:http';
import { getEventListeners } from 'node:events';
import { gzipSync, deflateSync, brotliCompressSync } from 'node:zlib';
import test from 'node:test';
import { fetchPublicWebPage, validatePublicWebUrl } from '../web-http-transport.mjs';

const PUBLIC_IP = '93.184.216.34';
const LIMIT = 2 * 1024 * 1024;
const address = (value = PUBLIC_IP, family = 4) => ({ address: value, family });

async function fixture(t, handler, lookup = async () => [address()]) {
  const seen = [], resolutions = [], server = createServer(handler);
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  // Only this explicit test dependency routes a validated public URL to an owned local fixture.
  // Production keeps the native request(URL, options) and its validated socket lookup.
  const request = (url, options, callback) => {
    const pin = new Promise((resolve, reject) => options.lookup(url.hostname, { all: true },
      (error, values) => error ? reject(error) : resolve(values)));
    seen.push({ url: url.href, options, pin });
    return requestHttp({ hostname: '127.0.0.1', port: server.address().port,
      path: url.pathname + url.search, method: options.method, headers: options.headers,
      agent: false, signal: options.signal }, callback);
  };
  return { seen, resolutions, dependencies: {
    lookup: async (hostname, options) => { resolutions.push({ hostname, options }); return lookup(hostname, options); },
    httpRequest: request, httpsRequest: request
  } };
}

test('pure URL validation rejects credentials, local/device schemes, ambiguous controls, private literals and custom ports', () => {
  const inputs = [
    '', 'https://example.com/ x', 'https://example.com/\nsecret', 'https://example.com/%0dsecret',
    'https:\\example.com', 'file:///C:/Private/data', '\\\\server\\share', 'javascript:alert(1)',
    'https://user:secret@example.com', 'https://@example.com', 'https://user%40x@example.com',
    'https://example.com:8080/', 'http://localhost/', 'http://LOCALHOST./', 'http://machine.local/',
    'http://127.0.0.1/', 'http://2130706433/', 'http://0x7f000001/', 'http://10.1.2.3/',
    'http://192.168.1.1/', 'http://169.254.169.254/', 'http://100.64.0.1/', 'http://198.18.0.1/',
    'http://192.0.2.1/', 'http://224.0.0.1/', 'http://[::1]/', 'http://[::ffff:127.0.0.1]/',
    'http://[2001:db8::1]/', 'http://[2002:0808:0808::1]/', 'http://[2001:0:1::1]/', 'http://[3fff::1]/'
  ];
  for (const input of inputs) assert.throws(() => validatePublicWebUrl(input), { code: 'WEB_URL_BLOCKED' });
});

test('pure URL validation returns a canonical URL without a fragment and permits public literals', () => {
  assert.equal(validatePublicWebUrl('HTTPS://Example.com:443/a?q=中文#part').href, 'https://example.com/a?q=%E4%B8%AD%E6%96%87');
  assert.equal(validatePublicWebUrl('http://8.8.8.8/').hostname, '8.8.8.8');
  assert.equal(validatePublicWebUrl('https://[2001:4860:4860::8888]/').hostname, '[2001:4860:4860::8888]');
});

test('every DNS answer must be public and family-correct before any request can start', async () => {
  const denied = ['0.1.2.3', '10.0.0.1', '100.64.0.1', '127.0.0.1', '169.254.169.254', '172.16.0.1',
    '192.0.0.1', '192.0.2.1', '192.88.99.1', '192.168.0.1', '198.18.0.1', '198.51.100.1',
    '203.0.113.1', '224.0.0.1', '255.255.255.255'];
  for (const value of denied) await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, {
    lookup: async () => [address(), address(value)], httpsRequest: () => assert.fail('Blocked DNS must never connect')
  }), { code: 'WEB_URL_BLOCKED' });
  for (const value of ['::1', 'fc00::1', 'fe80::1', '::ffff:8.8.8.8', '2001:db8::1', '2001::1', '2002::1', '3fff::1'])
    await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, {
      lookup: async () => [address(value, 6)], httpsRequest: () => assert.fail('Blocked IPv6 must never connect')
    }), { code: 'WEB_URL_BLOCKED' });
  for (const answers of [[], [address('invalid')], [address(PUBLIC_IP, 6)]])
    await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, {
      lookup: async () => answers, httpsRequest: () => assert.fail('Malformed DNS must never connect')
    }), { code: 'WEB_URL_BLOCKED' });
});

test('public GET preserves URL and pins verified DNS without importing cookies, authentication or response secrets', async t => {
  const f = await fixture(t, (request, response) => {
    assert.equal(request.method, 'GET'); assert.equal(request.headers.cookie, undefined); assert.equal(request.headers.authorization, undefined);
    response.writeHead(200, { 'content-type': 'text/html; charset=utf-8', 'set-cookie': 'PRIVATE_RESPONSE_COOKIE=secret',
      'www-authenticate': 'PRIVATE_AUTH', 'x-private': 'PRIVATE_HEADER' });
    response.end('<p>公开正文</p>');
  });
  const result = await fetchPublicWebPage('https://fixture.example/a?query=value#fragment', {}, f.dependencies);
  assert.equal(result.url, 'https://fixture.example/a?query=value'); assert.equal(result.status, 200);
  assert.deepEqual(result.headers, { contentType: 'text/html; charset=utf-8', contentEncoding: 'identity' });
  assert.equal(result.bytes.toString(), '<p>公开正文</p>'); assert.deepEqual(result.redirects, []);
  assert.deepEqual(await f.seen[0].pin, [address()]);
  assert.deepEqual(f.resolutions, [{ hostname: 'fixture.example', options: { all: true, verbatim: true } }]);
  assert.equal(f.seen[0].options.method, 'GET'); assert.equal(f.seen[0].options.agent, false);
  assert.equal(f.seen[0].options.rejectUnauthorized, undefined); assert.equal(f.seen[0].options.servername, undefined);
  assert.equal(f.seen[0].options.headers.Host, undefined); assert.doesNotMatch(JSON.stringify(result), /PRIVATE_/);
});

test('a DNS rebinding lookup cannot replace an already validated socket address', async t => {
  let calls = 0;
  const f = await fixture(t, (_request, response) => { response.writeHead(200, { 'content-type': 'text/plain' }); response.end('pinned'); },
    async () => ++calls === 1 ? [address(), address('2001:4860:4860::8888', 6)] : [address('127.0.0.1')]);
  const result = await fetchPublicWebPage('http://fixture.example/', {}, f.dependencies);
  assert.equal(result.bytes.toString(), 'pinned'); assert.equal(calls, 1);
  const options = f.seen[0].options;
  const v6 = await new Promise((resolve, reject) => options.lookup('fixture.example', { family: 6 },
    (error, value, family) => error ? reject(error) : resolve({ address: value, family })));
  assert.deepEqual(v6, address('2001:4860:4860::8888', 6)); assert.equal(calls, 1);
});

test('redirects validate each host and preserve only the final URL, safe headers and visited public URLs', async t => {
  const f = await fixture(t, (request, response) => {
    assert.equal(request.headers.cookie, undefined);
    if (request.url === '/first') { response.writeHead(302, { location: 'https://second.example/end', 'set-cookie': 'PRIVATE_SECRET' }); response.end(); }
    else { response.writeHead(200, { 'content-type': 'application/json' }); response.end('{"done":true}'); }
  });
  const result = await fetchPublicWebPage('http://first.example/first', {}, f.dependencies);
  assert.equal(result.url, 'https://second.example/end'); assert.deepEqual(result.redirects, ['https://second.example/end']);
  assert.deepEqual(f.resolutions.map(item => item.hostname), ['first.example', 'second.example']);
  assert.equal(f.seen.length, 2); assert.equal(result.bytes.toString(), '{"done":true}');
});

test('redirects to credentials, private literals, device URLs and nonstandard ports are rejected without connecting', async t => {
  for (const location of ['http://127.0.0.1/secret', 'https://user:PRIVATE_PASSWORD@public.example/',
    'file:///C:/Private/config', 'https://public.example:8080/', 'https://public.example/%0aevil']) {
    const f = await fixture(t, (_request, response) => { response.writeHead(302, { location }); response.end(); });
    await assert.rejects(fetchPublicWebPage('https://first.example/', {}, f.dependencies), { code: 'WEB_URL_BLOCKED' });
    assert.equal(f.seen.length, 1);
  }
});

test('redirect DNS cannot contain a private address even when the first answer is public', async t => {
  const f = await fixture(t, (_request, response) => { response.writeHead(302, { location: 'https://private.example/' }); response.end(); },
    async hostname => hostname === 'private.example' ? [address(), address('192.168.1.1')] : [address()]);
  await assert.rejects(fetchPublicWebPage('https://first.example/', {}, f.dependencies), { code: 'WEB_URL_BLOCKED' });
  assert.equal(f.seen.length, 1);
});

test('five redirects are supported and a sixth is stopped before another request', async t => {
  const f = await fixture(t, (request, response) => {
    const count = Number(new URL(request.url, 'http://fixture.example').searchParams.get('count') ?? 0);
    if (count < 5) { response.writeHead(302, { location: '/?count=' + (count + 1) }); response.end(); }
    else { response.writeHead(200, { 'content-type': 'text/plain' }); response.end('done'); }
  });
  const result = await fetchPublicWebPage('https://fixture.example/', {}, f.dependencies);
  assert.equal(result.redirects.length, 5); assert.equal(f.seen.length, 6);
  const endless = await fixture(t, (_request, response) => { response.writeHead(302, { location: '/again' }); response.end(); });
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', {}, endless.dependencies), { code: 'WEB_HTTP_ERROR', status: 302 });
  assert.equal(endless.seen.length, 6);
});

test('HTTP errors preserve the status and exclude response content and credentials', async t => {
  const f = await fixture(t, (_request, response) => {
    response.writeHead(403, { 'content-type': 'text/plain', 'set-cookie': 'PRIVATE_HTTP_COOKIE' }); response.end('PRIVATE_SERVER_BODY');
  });
  await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, f.dependencies), error => {
    assert.equal(error.code, 'WEB_HTTP_ERROR'); assert.equal(error.status, 403); assert.equal(error.httpStatus, 403);
    assert.doesNotMatch(error.message + JSON.stringify(error), /PRIVATE_/); return true;
  });
});

test('unsupported binary content and encodings fail with a stable code', async t => {
  for (const headers of [ { 'content-type': 'image/png' }, { 'content-type': 'application/octet-stream' },
    { 'content-type': 'text/plain', 'content-encoding': 'gzip, br' }, { 'content-type': 'text/plain', 'content-encoding': 'zstd' }, {} ]) {
    const f = await fixture(t, (_request, response) => { response.writeHead(200, headers); response.end('PRIVATE_UNSUPPORTED_CONTENT'); });
    await assert.rejects(fetchPublicWebPage('http://fixture.example/', {}, f.dependencies), { code: 'WEB_UNSUPPORTED_CONTENT' });
  }
});

test('gzip, deflate and Brotli decode bounded streamed text and XML remains supported', async t => {
  const original = Buffer.from('<message>中文 and English</message>');
  for (const [encoding, compress] of [['gzip', gzipSync], ['deflate', deflateSync], ['br', brotliCompressSync]]) {
    const f = await fixture(t, (_request, response) => {
      response.writeHead(200, { 'content-type': 'application/example+xml', 'content-encoding': encoding });
      const bytes = compress(original); response.write(bytes.subarray(0, 5)); response.end(bytes.subarray(5));
    });
    const result = await fetchPublicWebPage('https://fixture.example/', {}, f.dependencies);
    assert.deepEqual(result.bytes, original); assert.equal(result.headers.contentEncoding, encoding);
  }
});

test('raw and decompressed responses cannot exceed 2 MiB, including a gzip expansion bomb', async t => {
  for (const compressed of [false, true]) {
    const oversized = Buffer.alloc(LIMIT + 1, 'x');
    const f = await fixture(t, (_request, response) => {
      response.writeHead(200, { 'content-type': 'text/plain', ...(compressed ? { 'content-encoding': 'gzip' } : {}) });
      response.write(compressed ? gzipSync(oversized) : oversized); response.end();
    });
    await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, f.dependencies), { code: 'WEB_RESPONSE_TOO_LARGE' });
  }
  const declared = await fixture(t, (_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain', 'content-length': String(LIMIT + 1) }); response.end();
  });
  await assert.rejects(fetchPublicWebPage('https://fixture.example/', {}, declared.dependencies), { code: 'WEB_RESPONSE_TOO_LARGE' });
});

test('pre-cancelled calls do not resolve DNS and cancellation stops a streaming body without leaking the reason', async t => {
  const controller = new AbortController(); controller.abort(new Error('PRIVATE_CANCEL_REASON'));
  let dnsCalls = 0;
  await assert.rejects(fetchPublicWebPage('https://fixture.example/', { signal: controller.signal }, {
    lookup: async () => { dnsCalls++; return [address()]; }
  }), { name: 'AbortError', code: 'ABORT_ERR' });
  assert.equal(dnsCalls, 0);
  assert.equal(getEventListeners(controller.signal, 'abort').length, 0);
  const streaming = new AbortController();
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const f = await fixture(t, (_request, response) => { response.writeHead(200, { 'content-type': 'text/plain' }); response.write('start'); started(); });
  const pending = fetchPublicWebPage('https://fixture.example/', { signal: streaming.signal }, f.dependencies);
  await ready; streaming.abort(new Error('PRIVATE_CANCEL_REASON'));
  await assert.rejects(pending, error => { assert.equal(error.code, 'ABORT_ERR'); assert.doesNotMatch(error.message, /PRIVATE_/); return true; });
  assert.equal(getEventListeners(streaming.signal, 'abort').length, 0);
});

test('the same deadline covers pending DNS, response headers and a stalled body', async t => {
  const f = await fixture(t, (_request, response) => { response.writeHead(200, { 'content-type': 'text/plain' }); response.write('partial'); });
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', { timeoutMs: 25 }, f.dependencies), { code: 'WEB_TIMEOUT' });
  const headers = await fixture(t, () => {});
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', { timeoutMs: 25 }, headers.dependencies), { code: 'WEB_TIMEOUT' });
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', { timeoutMs: 25 }, {
    lookup: () => new Promise(() => {}), httpRequest: () => assert.fail('Pending DNS cannot connect')
  }), { code: 'WEB_TIMEOUT' });
});

test('broken compression and early disconnect are errors rather than successful partial content', async t => {
  const compressed = await fixture(t, (_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain', 'content-encoding': 'gzip' }); response.end('PRIVATE_BAD_GZIP');
  });
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', {}, compressed.dependencies), { code: 'WEB_HTTP_ERROR' });
  const truncated = await fixture(t, (_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain', 'content-length': '1000' }); response.write('partial');
    setImmediate(() => response.socket.destroy());
  });
  await assert.rejects(fetchPublicWebPage('http://fixture.example/', {}, truncated.dependencies), { code: 'WEB_HTTP_ERROR' });
});
