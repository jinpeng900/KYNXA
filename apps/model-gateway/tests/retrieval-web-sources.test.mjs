import assert from 'node:assert/strict';
import { test } from 'node:test';
import { normalizeWebSources } from '../tools/retrieval/web-source-normalizer.mjs';

test('Exa source blocks retain their own titles and content without neighbouring evidence', () => {
  const text = [
    'Title: Alpha official release',
    'URL: https://example.com/alpha',
    'Published Date: 2026-01-02T00:00:00Z',
    'Author: Alpha team',
    'Content: Alpha-only release details.',
    'The alpha guide references https://example.com/guide.',
    '', '---', '',
    'Title: Beta independent report',
    'URL: https://example.org/beta',
    'Published: N/A',
    'Author: Beta team',
    'Highlights:',
    'Beta-only report details.'
  ].join('\n');
  const sources = normalizeWebSources({ content: [{ type: 'text', text }] });
  assert.deepEqual(sources, [
    { url: 'https://example.com/alpha', title: 'Alpha official release',
      excerpt: 'Alpha-only release details.\nThe alpha guide references https://example.com/guide.' },
    { url: 'https://example.org/beta', title: 'Beta independent report', excerpt: 'Beta-only report details.' }
  ]);
  assert.ok(!sources[0].excerpt.includes('Beta'));
  assert.ok(!sources[1].excerpt.includes('Alpha'));
});

test('consecutive labelled sources and CRLF bodies work without separators or optional metadata', () => {
  const text = 'Title: First\r\nURL: https://example.com/one\r\nText:\r\nFirst body.\r\n' +
    'Title: Second\r\nURL: https://example.com/two\r\nContent: Second body.';
  assert.deepEqual(normalizeWebSources(text).map(source => [source.title, source.excerpt]),
    [['First', 'First body.'], ['Second', 'Second body.']]);
});

test('URL-only labelled blocks stay separate and never become shared excerpt windows', () => {
  const text = 'URL: https://example.com/one\nFirst body.\nURL: https://example.com/two\nSecond body.';
  assert.deepEqual(normalizeWebSources(text), [
    { url: 'https://example.com/one', title: '', excerpt: 'First body.' },
    { url: 'https://example.com/two', title: '', excerpt: 'Second body.' }
  ]);
});

test('structured sources accept JSON wrappers and retain arrays of provider highlights', () => {
  const input = JSON.stringify({ structuredContent: { results: [
    { url: 'https://example.com/one', title: 'Structured one', text: 'Own text with https://example.org/reference' },
    { url: 'https://example.com/two', name: 'Structured two', highlights: ['First highlight.', 'Second highlight.'] }
  ] } });
  assert.deepEqual(normalizeWebSources(input), [
    { url: 'https://example.com/one', title: 'Structured one', excerpt: 'Own text with https://example.org/reference' },
    { url: 'https://example.com/two', title: 'Structured two', excerpt: 'First highlight.\nSecond highlight.' }
  ]);
});

test('unlabelled links provide readable titles without borrowing nearby prose as evidence', () => {
  const text = 'Alpha prose must not be attributed to beta.\n[Alpha title](https://example.com/alpha)\n' +
    'https://example.org/beta\nBeta prose must not be attributed to alpha.';
  assert.deepEqual(normalizeWebSources(text), [
    { url: 'https://example.com/alpha', title: 'Alpha title', excerpt: '' },
    { url: 'https://example.org/beta', title: '', excerpt: '' }
  ]);
});

test('duplicate URLs merge missing metadata within the source limit without changing source order', () => {
  const input = { content: [{ type: 'text', text: 'https://example.com/one#section\nhttps://example.com/two' }],
    structuredContent: { results: [
      { url: 'https://example.com/one', title: 'Authoritative one', description: 'Authoritative excerpt.' },
      { url: 'https://example.com/three', title: 'Must be excluded', text: 'Outside limit.' }
    ] } };
  assert.deepEqual(normalizeWebSources(input, 2), [
    { url: 'https://example.com/one', title: 'Authoritative one', excerpt: 'Authoritative excerpt.' },
    { url: 'https://example.com/two', title: '', excerpt: '' }
  ]);
  assert.deepEqual(normalizeWebSources(input, 0), []);
});

test('private, credentialed, unsupported and invalid URLs never become public sources', () => {
  const rejected = ['http://127.0.0.1/private', 'http://localhost/private', 'http://10.0.0.1/private',
    'https://user:synthetic-password@example.com/private', 'file:///private', 'https://example.com:444/private'];
  const text = rejected.map((url, index) => `Title: Rejected ${index}\nURL: ${url}\nContent: Private block.`).join('\n---\n') +
    '\n---\nTitle: Public\nURL: https://example.org/public\nContent: Public body.';
  assert.deepEqual(normalizeWebSources(text), [
    { url: 'https://example.org/public', title: 'Public', excerpt: 'Public body.' }
  ]);
  assert.deepEqual(normalizeWebSources(rejected.map(url => ({ url, title: 'Rejected' }))), []);
});

test('empty blocks do not steal the next title and metadata is excluded from excerpts', () => {
  const text = 'Title: Empty block without URL\n---\nTitle: Complete block\nURL: https://example.com/complete\n' +
    'Published Date: 2026-01-01\nAuthor: Source author\nContent: Only the actual body.';
  assert.deepEqual(normalizeWebSources(text), [
    { url: 'https://example.com/complete', title: 'Complete block', excerpt: 'Only the actual body.' }
  ]);
});

test('source titles and excerpts remain bounded and private MCP metadata is ignored', () => {
  const source = { url: 'https://example.com/bounded', title: 'T'.repeat(800), text: 'E'.repeat(4000) };
  const result = normalizeWebSources({ structuredContent: { results: [source] },
    _meta: { url: 'https://example.com/private-metadata', text: 'Must remain private.' },
    content: [{ type: 'image', data: 'https://example.com/binary-field' }] });
  assert.equal(result.length, 1);
  assert.equal(result[0].title.length, 512);
  assert.equal(result[0].excerpt.length, 2500);
});

test('cyclic or deeply nested provider objects terminate without inventing evidence', () => {
  const input = { result: { url: 'https://example.com/finite', title: 'Finite', snippet: 'Own finite snippet.' } };
  input.circular = input;
  assert.deepEqual(normalizeWebSources(input), [
    { url: 'https://example.com/finite', title: 'Finite', excerpt: 'Own finite snippet.' }
  ]);
  let nested = { url: 'https://example.com/too-deep', title: 'Excluded by depth' };
  for (let index = 0; index < 14; index++) nested = { nested };
  assert.deepEqual(normalizeWebSources(nested), []);
});
