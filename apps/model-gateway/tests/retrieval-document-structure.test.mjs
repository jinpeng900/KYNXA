import test from 'node:test';
import assert from 'node:assert/strict';
import { parseDocumentStructure, MARKDOWN_PARSER_VERSION, TEXT_PARSER_VERSION } from '../data/retrieval/document-structure.mjs';
import { chunkSource, chunkStructuredSource, embeddingTextForChunk,
  CHUNKER_VERSION, STRUCTURED_CHUNKER_VERSION } from '../data/retrieval/retrieval-text.mjs';
import { hashText } from '../data/retrieval/retrieval-contracts.mjs';
import { sourceWindow } from '../data/retrieval/source-window.mjs';

const source = (text, path = 'document.md') => ({ sourceId: 'synthetic-structure', scopeKey: 'user', sourceType: 'knowledge',
  title: 'Synthetic document', text, sourceRevision: 1, locator: { relativePath: path } });

function verifyCoverage(text, chunks, maximumChars = 384) {
  const covered = new Uint8Array(text.length);
  let previousEnd = 0, offset = 0, line = 1;
  const lineAt = position => {
    while (offset < position) if (text[offset++] === '\n') line++;
    return line;
  };
  for (const [index, chunk] of chunks.entries()) {
    assert.equal(chunk.chunkIndex, index);
    assert.ok(chunk.startOffset >= previousEnd);
    assert.ok(chunk.text.length <= maximumChars);
    assert.equal(chunk.text, text.slice(chunk.startOffset, chunk.endOffset));
    assert.equal(chunk.chunkHash, hashText(chunk.text));
    assert.equal(chunk.startLine, lineAt(chunk.startOffset));
    assert.equal(chunk.endLine, lineAt(chunk.endOffset));
    assert.equal(chunk.structure.unitStartOffset <= chunk.startOffset, true);
    assert.equal(chunk.structure.unitEndOffset >= chunk.endOffset, true);
    assert.equal(chunk.chunkerVersion, STRUCTURED_CHUNKER_VERSION);
    if (chunk.startOffset > 0 && chunk.startOffset < text.length)
      assert.equal(/[\uD800-\uDBFF]/u.test(text[chunk.startOffset - 1]) && /[\uDC00-\uDFFF]/u.test(text[chunk.startOffset]), false);
    if (chunk.endOffset < text.length)
      assert.equal(/[\uD800-\uDBFF]/u.test(text[chunk.endOffset - 1]) && /[\uDC00-\uDFFF]/u.test(text[chunk.endOffset]), false);
    for (let character = chunk.startOffset; character < chunk.endOffset; character++) {
      assert.equal(covered[character], 0);
      covered[character] = 1;
    }
    previousEnd = chunk.endOffset;
  }
  for (let character = 0; character < text.length; character++)
    if (/\S/u.test(text[character])) assert.equal(covered[character], 1);
}

test('Markdown ATX headings preserve UTF-16 offsets, hierarchy and CRLF while fenced headings remain code', () => {
  const text = '🌏 introduction\r\n# Top\r\nBody.\r\n## Child\r\n```md\r\n# Fake heading\r\n```\r\n# Next\r\nDone.\r\n';
  const parsed = parseDocumentStructure(source(text));
  assert.equal(parsed.parserVersion, MARKDOWN_PARSER_VERSION);
  assert.equal(parsed.parseStatus, 'parsed');
  const headings = parsed.units.filter(unit => unit.kind === 'heading');
  assert.deepEqual(headings.map(unit => unit.sectionTitle), ['Top', 'Child', 'Next']);
  assert.deepEqual(headings[1].sectionPath, ['Top', 'Child']);
  assert.deepEqual(headings[2].sectionPath, ['Next']);
  assert.equal(headings[0].startOffset, text.indexOf('# Top'));
  assert.equal(headings[0].startLine, 2);
  const fenced = parsed.units.find(unit => unit.kind === 'code-block');
  assert.equal(text.slice(fenced.startOffset, fenced.endOffset), '```md\r\n# Fake heading\r\n```\r\n');
  verifyCoverage(text, chunkStructuredSource(source(text), parsed));
});

test('single-line Setext headings share existing section navigation without treating preceding text as a title', () => {
  const text = 'Title\n=====\nPublic text.\nSubsection\n----------\nMore text.\n';
  const parsed = parseDocumentStructure(source(text));
  assert.deepEqual(parsed.units.filter(unit => unit.kind === 'heading').map(unit => unit.sectionTitle), ['Title', 'Subsection']);
  assert.ok(parsed.units.some(unit => unit.kind === 'paragraph' && text.slice(unit.startOffset, unit.endOffset) === 'Public text.\n'));
  assert.equal(sourceWindow(text, { mode: 'section', anchorOffset: text.indexOf('More text') }).window.section.title, 'Subsection');
  verifyCoverage(text, chunkStructuredSource(source(text), parsed));
});

test('tables have distinct full-unit ranges and do not consume the following paragraph or section', () => {
  const text = '# Results\n| Name | Value |\n| :--- | ---: |\n| a | 10 |\n| b | 20 |\n\nFollowing paragraph.\n# End\n';
  const parsed = parseDocumentStructure(source(text));
  const table = parsed.units.find(unit => unit.kind === 'table');
  assert.ok(table);
  assert.equal(table.sectionTitle, 'Results');
  assert.equal(text.slice(table.startOffset, table.endOffset), '| Name | Value |\n| :--- | ---: |\n| a | 10 |\n| b | 20 |\n');
  assert.ok(parsed.units.some(unit => unit.kind === 'paragraph' && text.slice(unit.startOffset, unit.endOffset).includes('Following')));
  verifyCoverage(text, chunkStructuredSource(source(text), parsed));
});

test('an unterminated code fence is a partial parse and never invents a chapter inside it', () => {
  const text = '# Genuine\n~~~md\n# Fake\nNot closed.\n';
  const parsed = parseDocumentStructure(source(text));
  assert.equal(parsed.parseStatus, 'partial');
  assert.deepEqual(parsed.diagnosticCodes, ['UNTERMINATED_CODE_FENCE']);
  assert.deepEqual(parsed.units.filter(unit => unit.kind === 'heading').map(unit => unit.sectionTitle), ['Genuine']);
  assert.equal(parsed.units.at(-1).endOffset, text.length);
  verifyCoverage(text, chunkStructuredSource(source(text), parsed));
});

test('plain text paragraphs keep Markdown-like text literal and retain their actual offsets', () => {
  const text = '# Literal text\r\nNext line.\r\n\r\nSecond paragraph.\r\n';
  const parsed = parseDocumentStructure(source(text, 'plain.txt'));
  assert.equal(parsed.language, 'text');
  assert.equal(parsed.parserVersion, TEXT_PARSER_VERSION);
  assert.deepEqual(parsed.units.map(unit => unit.kind), ['paragraph', 'paragraph']);
  assert.equal(parsed.units[1].startOffset, text.indexOf('Second'));
  verifyCoverage(text, chunkStructuredSource(source(text, 'plain.txt'), parsed));
});

test('small sections stay separate and legacy synchronous chunking retains its original version', () => {
  const text = '# Alpha\nOne.\n# Beta\nTwo.\n';
  const input = source(text), parsed = parseDocumentStructure(input);
  const chunks = chunkStructuredSource(input, parsed);
  assert.equal(chunks.length, 4);
  assert.ok(chunks.every(chunk => !(chunk.text.includes('Alpha') && chunk.text.includes('Beta'))));
  assert.deepEqual(chunks, chunkStructuredSource(input, parsed));
  assert.ok(chunkSource(input).every(chunk => chunk.chunkerVersion === CHUNKER_VERSION && chunk.structure === undefined));
});

test('large code units retain complete true function ranges and unparsed imports or separators get honest context', () => {
  const alpha = `function alpha() {\n${'  computeAlpha();\n'.repeat(70)}}\n`;
  const beta = 'function beta() { return 2; }\n';
  const text = `import moduleName;\n${alpha}\n${beta}// trailer\n`;
  const alphaStart = text.indexOf('function alpha'), betaStart = text.indexOf('function beta');
  const input = source(text, 'module.js');
  const parsed = { domain: 'code', language: 'javascript', parserVersion: 'synthetic-code-v1', parseStatus: 'parsed', diagnosticCodes: [],
    units: [{ kind: 'function', symbolName: 'alpha', qualifiedName: 'Module.alpha', parentSymbol: 'Module',
      startOffset: alphaStart, endOffset: alphaStart + alpha.length },
    { kind: 'function', symbolName: 'beta', qualifiedName: 'Module.beta', parentSymbol: 'Module',
      startOffset: betaStart, endOffset: betaStart + beta.length }] };
  const chunks = chunkStructuredSource(input, parsed);
  const alphaChunks = chunks.filter(chunk => chunk.structure.symbolName === 'alpha');
  assert.ok(alphaChunks.length > 1);
  assert.ok(alphaChunks.every(chunk => chunk.structure.unitStartOffset === alphaStart &&
    chunk.structure.unitEndOffset === alphaStart + alpha.length && !chunk.text.includes('function beta')));
  assert.ok(chunks.filter(chunk => chunk.structure.kind === 'context').every(chunk => chunk.structure.symbolName === undefined));
  verifyCoverage(text, chunks);
});

test('bounded long-unit splitting covers emoji and long newline-free paragraphs without cutting surrogate pairs', () => {
  const text = `# Emoji\r\n${'🌏'.repeat(600)}\r\n\r\n${'中文字符'.repeat(12000)}`;
  const input = source(text), parsed = parseDocumentStructure(input);
  verifyCoverage(text, chunkStructuredSource(input, parsed));
});

test('unavailable code parsing still covers raw text without fake Markdown sections or guessed symbols', () => {
  const input = source('const value = "# Fake heading";\nunknown syntax\n', 'unsupported.xyz');
  const parsed = { domain: 'code', language: null, parserVersion: 'unavailable-code-v1', parseStatus: 'unavailable',
    diagnosticCodes: ['CODE_STRUCTURE_UNAVAILABLE'], units: [] };
  const chunks = chunkStructuredSource(input, parsed);
  assert.ok(chunks.every(chunk => chunk.structure.kind === 'context' && chunk.structure.parseStatus === 'unavailable'));
  assert.doesNotMatch(embeddingTextForChunk(input, chunks[0]), /Section:|Symbol:/u);
  verifyCoverage(input.text, chunks);
});

test('structural labels enrich bounded embedding input while cited text remains unchanged', () => {
  const input = source('# Genuine section\nRelevant paragraph.\n');
  const chunks = chunkStructuredSource(input, parseDocumentStructure(input));
  const body = chunks.find(chunk => chunk.structure.kind === 'paragraph');
  const embedding = embeddingTextForChunk(input, body);
  assert.match(embedding, /Section: Genuine section/u);
  assert.ok(embedding.endsWith(body.text));
  assert.equal(body.text, 'Relevant paragraph.\n');
  assert.equal(body.chunkHash, hashText(body.text));
  assert.ok(embedding.length <= 512);
});

test('invalid, overlapping or surrogate-splitting units are rejected before publication', () => {
  const input = source('🌏abcdef\n');
  const base = { domain: 'code', language: 'javascript', parseStatus: 'parsed', units: [] };
  for (const units of [[null], [{ kind: 'function', startOffset: 1, endOffset: 5 }],
    [{ kind: 'function', startOffset: 0, endOffset: 5 }, { kind: 'function', startOffset: 4, endOffset: 8 }],
    [{ kind: 'function', startOffset: 0, endOffset: 99 }]])
    assert.throws(() => chunkStructuredSource(input, { ...base, units }), { code: 'INVALID_RETRIEVAL_STRUCTURE' });
});

test('parent declaration fragments cover raw code once while preserving the full outer read-back range', () => {
  const text = "const label = '🌏';\r\nfunction outer() {\r\n  const before = 1;\r\n  function inner() { return before; }\r\n  return inner();\r\n}\r\n";
  const input = source(text, 'nested.js');
  const outerStart = text.indexOf('function outer'), innerStart = text.indexOf('function inner');
  const innerEnd = text.indexOf('}', innerStart) + 1, outerEnd = text.length;
  const outerFragment = (startOffset, endOffset) => ({ kind: 'function', symbolName: 'outer',
    startOffset, endOffset, unitStartOffset: outerStart, unitEndOffset: outerEnd });
  const units = [outerFragment(outerStart, innerStart), { kind: 'function', symbolName: 'inner', parentSymbol: 'outer',
    startOffset: innerStart, endOffset: innerEnd }, outerFragment(innerEnd, outerEnd)]
    .map(unit => ({ ...unit, startLine: 1 + (text.slice(0, unit.startOffset).match(/\n/gu)?.length ?? 0),
      endLine: 1 + (text.slice(0, unit.endOffset).match(/\n/gu)?.length ?? 0) }));
  const parsed = { domain: 'code', language: 'javascript', parseStatus: 'parsed', units };
  const chunks = chunkStructuredSource(input, parsed);
  const outerChunks = chunks.filter(chunk => chunk.structure.symbolName === 'outer');
  assert.equal(outerChunks.length, 2);
  assert.ok(outerChunks.every(chunk => chunk.structure.unitStartOffset === outerStart &&
    chunk.structure.unitEndOffset === outerEnd && !chunk.text.includes('function inner')));
  assert.ok(chunks.filter(chunk => chunk.structure.symbolName === 'inner')
    .every(chunk => chunk.structure.unitStartOffset === innerStart && chunk.structure.unitEndOffset === innerEnd));
  verifyCoverage(text, chunks);
  for (const invalid of [{ unitStartOffset: null }, { unitStartOffset: innerStart }, { unitEndOffset: innerStart - 1 },
    { unitStartOffset: text.indexOf('🌏') + 1 }, { unitEndOffset: text.length + 1 }, { startLine: 99 }, { endLine: 99 }])
    assert.throws(() => chunkStructuredSource(input, { ...parsed, units: [{ ...units[0], ...invalid }, ...units.slice(1)] }),
      { code: 'INVALID_RETRIEVAL_STRUCTURE' });
});

test('long-document structure scans and chunk assembly observe cancellation without changing raw source text', () => {
  const input = source('A public line.\n'.repeat(3000), 'plain.txt');
  let checks = 0;
  assert.throws(() => parseDocumentStructure(input, { checkCancelled: () => {
    if (++checks === 3) throw Object.assign(new Error('Synthetic cancelled.'), { name: 'AbortError' });
  } }), { name: 'AbortError' });
  const parsed = parseDocumentStructure(input);
  assert.throws(() => chunkStructuredSource(input, parsed, { checkCancelled: () => {
    throw Object.assign(new Error('Synthetic cancelled.'), { name: 'AbortError' });
  } }), { name: 'AbortError' });
  assert.equal(input.text, 'A public line.\n'.repeat(3000));
});

test('dense headings stop at the unit budget and preserve the complete unparsed suffix as raw context', () => {
  const text = '# Heading\n\n'.repeat(6000) + 'Final evidence remains readable.\n';
  const input = source(text), parsed = parseDocumentStructure(input);
  assert.equal(parsed.parseStatus, 'partial');
  assert.ok(parsed.diagnosticCodes.includes('DOCUMENT_STRUCTURE_LIMIT'));
  assert.ok(parsed.units.length <= 5000);
  const chunks = chunkStructuredSource(input, parsed);
  assert.ok(chunks.some(chunk => chunk.structure.kind === 'context' && chunk.text.includes('Final evidence')));
  verifyCoverage(text, chunks);
});

test('line and elapsed-time budgets return explicit partial coverage with unchanged raw evidence', () => {
  const text = '\n'.repeat(100_001) + 'Tail evidence.\n', input = source(text, 'plain.txt');
  const lineLimited = parseDocumentStructure(input);
  assert.equal(lineLimited.parseStatus, 'partial');
  assert.ok(lineLimited.diagnosticCodes.includes('DOCUMENT_STRUCTURE_LIMIT'));
  verifyCoverage(text, chunkStructuredSource(input, lineLimited));
  const descriptor = Object.getOwnPropertyDescriptor(performance, 'now');
  let elapsedMs = 0;
  Object.defineProperty(performance, 'now', { configurable: true, value: () => elapsedMs += 100 });
  let timed;
  try { timed = parseDocumentStructure(source('A paragraph.\n'.repeat(3000), 'plain.txt')); }
  finally {
    if (descriptor) Object.defineProperty(performance, 'now', descriptor);
    else delete performance.now;
  }
  assert.equal(timed.parseStatus, 'partial');
  assert.ok(timed.diagnosticCodes.includes('DOCUMENT_STRUCTURE_LIMIT'));
  assert.ok(timed.units.length <= 5000);
  const timedInput = source('A paragraph.\n'.repeat(3000), 'plain.txt');
  verifyCoverage(timedInput.text, chunkStructuredSource(timedInput, timed));
});
