import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { codeLanguageForSource, parseCodeStructure, CODE_PARSER_VERSION } from '../data/retrieval/code-structure.mjs';
import { chunkStructuredSource } from '../data/retrieval/retrieval-text.mjs';

const require = createRequire(import.meta.url);
const source = (filename, text, extra = {}) => ({ sourceId: 'synthetic-code', scopeKey: 'user', sourceType: 'work-file', title: filename,
  locator: { relativePath: filename }, text, ...extra });

function assertRanges(text, result) {
  let end = 0, offset = 0, line = 1;
  for (const unit of result.units) {
    assert.ok(unit.startOffset >= end);
    assert.ok(unit.endOffset > unit.startOffset && unit.endOffset <= text.length);
    assert.ok(unit.unitStartOffset <= unit.startOffset && unit.unitEndOffset >= unit.endOffset);
    assert.ok(unit.unitStartOffset >= 0 && unit.unitEndOffset <= text.length);
    assert.ok(text.slice(unit.startOffset, unit.endOffset).trim());
    for (; offset < unit.startOffset; offset++) if (text[offset] === '\n') line++;
    assert.equal(unit.startLine, line);
    for (; offset < unit.endOffset; offset++) if (text[offset] === '\n') line++;
    assert.equal(unit.endLine, line);
    assert.ok(!(/[\uD800-\uDBFF]/u.test(text[unit.startOffset - 1] ?? '') && /[\uDC00-\uDFFF]/u.test(text[unit.startOffset] ?? '')));
    assert.ok(!(/[\uD800-\uDBFF]/u.test(text[unit.endOffset - 1] ?? '') && /[\uDC00-\uDFFF]/u.test(text[unit.endOffset] ?? '')));
    end = unit.endOffset;
  }
}

test('packaged JavaScript grammar loads offline and ignores function-like comments and strings', async () => {
  const text = ['// 😀 function Ghost() { }', 'const explanation = "function StringGhost() {}";',
    'export function visible(value) { return value + 1; }', 'const arrow = (value) => value * 2;'].join('\r\n');
  const originalFetch = globalThis.fetch;
  let fetches = 0;
  globalThis.fetch = async () => { fetches++; throw new Error('Structure parsing must not fetch resources.'); };
  try {
    const result = await parseCodeStructure(source('sample.mjs', text));
    assert.equal(result.parseStatus, 'parsed');
    assert.equal(result.language, 'javascript');
    assert.equal(result.parserVersion, `${CODE_PARSER_VERSION}:javascript`);
    assert.deepEqual(result.diagnosticCodes, []);
    assert.deepEqual(result.units.map(unit => unit.symbolName), ['visible', 'arrow']);
    assert.equal(text.slice(result.units[0].startOffset, result.units[0].endOffset), 'function visible(value) { return value + 1; }');
    assert.match(text.slice(result.units[1].startOffset, result.units[1].endOffset), /^arrow\s*=/);
    assertRanges(text, result);
    assert.equal(fetches, 0);
  } finally { globalThis.fetch = originalFetch; }
});

test('C# generics, overloads, namespaces and nested functions retain exact UTF16 ranges and parents', async () => {
  const text = ['// 😀 namespace Fake { class StringFake { } }', 'extern alias Dependency;', 'using System;',
    'namespace Example.Core;', 'public class Box<T>', '{', '  public Box() { }', '  public T Value { get; set; }',
    '  public int Read(int value) => value;', '  public int Read(string value) => value.Length;',
    '  public U Convert<U>(T value, U other) => other;', '  public int Outer(int value)', '  {',
    '    int Inner(int input) { return input; }', '    return Inner(value);', '  }', '}'].join('\r\n');
  const result = await parseCodeStructure(source('Box.cs', text));
  assert.equal(result.parseStatus, 'parsed');
  assert.equal(result.language, 'csharp');
  assert.equal(result.units.filter(unit => unit.symbolName === 'Read').length, 2);
  assert.ok(result.units.some(unit => unit.kind === 'constructor' && unit.parentSymbol === 'Example.Core.Box'));
  const nested = result.units.find(unit => unit.symbolName === 'Inner');
  assert.equal(nested.qualifiedName, 'Example.Core.Box.Outer.Inner');
  assert.equal(nested.parentSymbol, 'Example.Core.Box.Outer');
  const box = result.units.filter(unit => unit.kind === 'class' && unit.symbolName === 'Box');
  assert.ok(box.length >= 2, 'parent class headers and tails keep their own indexed symbol');
  assert.ok(box.some(unit => text.slice(unit.startOffset, unit.endOffset).includes('public class Box<T>')));
  assert.ok(box.every(unit => unit.unitStartOffset === text.indexOf('public class Box<T>') && unit.unitEndOffset === text.length));
  assert.ok(result.units.filter(unit => unit.symbolName === 'Outer').length >= 2);
  assert.ok(result.units.some(unit => unit.symbolName === 'Convert' && unit.qualifiedName === 'Example.Core.Box.Convert'));
  assert.equal(result.units.some(unit => unit.symbolName === 'Fake' || unit.symbolName === 'StringFake'), false);
  assertRanges(text, result);
});

test('modern C# collection expressions, primary constructors, generic records and raw strings parse original text offline', async () => {
  const text = ['// 😀 modern syntax keeps UTF16 offsets', 'namespace Example.Modern;',
    'public sealed record Envelope<T>(T Value) where T : class;',
    'public sealed class Store<T>(string name) where T : class', '{',
    '  private readonly List<T> _empty = [];',
    '  public int[] Values => [1, 2, 3];',
    '  public int[] Spread(int[] values) => [0, ..values, 4];',
    '  public U Convert<U>(U value) => value;',
    '  public string Read() => """',
    '    class Ghost { void Fake() { } } [] 😀',
    '    """;',
    '  public string Template() => $$"""{{name}} [1] class StringGhost {}""";', '}'].join('\r\n');
  const input = source('Store.cs', text), originalFetch = globalThis.fetch;
  let fetches = 0;
  globalThis.fetch = async () => { fetches++; throw new Error('Structure parsing must not fetch resources.'); };
  try {
    const result = await parseCodeStructure(input);
    assert.equal(result.parseStatus, 'parsed');
    assert.deepEqual(result.diagnosticCodes, []);
    assert.match(result.parserVersion, /csharp-0\.23\.5/);
    assert.equal(input.text, text);
    assert.ok(result.units.some(unit => unit.kind === 'record' && unit.qualifiedName === 'Example.Modern.Envelope'));
    const store = result.units.filter(unit => unit.qualifiedName === 'Example.Modern.Store');
    assert.ok(store.length >= 2);
    assert.ok(store.every(unit => unit.unitStartOffset === text.indexOf('public sealed class Store<T>(string name)') &&
      unit.unitEndOffset === text.length));
    for (const symbolName of ['Values', 'Spread', 'Convert', 'Read', 'Template']) {
      const unit = result.units.find(item => item.symbolName === symbolName);
      assert.equal(unit.parentSymbol, 'Example.Modern.Store');
      assert.equal(unit.qualifiedName, `Example.Modern.Store.${symbolName}`);
    }
    const spread = result.units.find(unit => unit.symbolName === 'Spread');
    assert.equal(text.slice(spread.startOffset, spread.endOffset), 'public int[] Spread(int[] values) => [0, ..values, 4];');
    assert.equal(result.units.some(unit => ['Ghost', 'Fake', 'StringGhost'].includes(unit.symbolName)), false);
    assertRanges(text, result);
    assert.equal(fetches, 0);
  } finally { globalThis.fetch = originalFetch; }
});

test('the real ProjectStore source retains its class and method ranges with a collection initializer', async () => {
  const text = await readFile(new URL('../../desktop/Services/Data/ProjectStore.cs', import.meta.url), 'utf8');
  const input = source('ProjectStore.cs', text), result = await parseCodeStructure(input);
  assert.equal(result.parseStatus, 'parsed');
  assert.deepEqual(result.diagnosticCodes, []);
  assert.equal(input.text, text);
  const projectStore = result.units.filter(unit => unit.qualifiedName === 'KYNXA_Desktop.Services.ProjectStore');
  assert.ok(projectStore.length >= 2);
  assert.ok(projectStore.every(unit => unit.unitStartOffset === text.indexOf('public sealed class ProjectStore(') &&
    unit.unitEndOffset === text.lastIndexOf('}') + 1));
  assert.ok(result.units.some(unit => unit.symbolName === 'LoadAsync' && unit.parentSymbol === 'KYNXA_Desktop.Services.ProjectStore'));
  assert.ok(result.units.some(unit => unit.symbolName === 'ReadAsync' && unit.parentSymbol === 'KYNXA_Desktop.Services.ProjectStore'));
  assertRanges(text, result);
});

test('modern C# still reports genuine collection syntax errors and retains independently valid methods', async () => {
  const text = 'namespace Example; class Broken(string name) { int[] Values => [1, ???]; int Good() => 2; }';
  const result = await parseCodeStructure(source('Broken.cs', text));
  assert.equal(result.parseStatus, 'partial');
  assert.ok(result.diagnosticCodes.includes('CODE_SYNTAX_ERROR'));
  assert.ok(result.units.some(unit => unit.symbolName === 'Good' && unit.qualifiedName === 'Example.Broken.Good'));
  assert.equal(result.units.some(unit => unit.symbolName === 'Values'), false);
  assertRanges(text, result);
});

test('JavaScript class and bound-object methods keep nested ownership without overlapping parent bodies', async () => {
  const text = 'class Box { constructor() {} run(value) { function inner(input) { return input; } return inner(value); } }\n' +
    'const object = { read(value) { return value; }, nested: (value) => value };\nclass Empty {}';
  const result = await parseCodeStructure(source('sample.jsx', text));
  assert.equal(result.parseStatus, 'parsed');
  assert.ok(result.units.some(unit => unit.kind === 'constructor' && unit.qualifiedName === 'Box.constructor'));
  assert.equal(result.units.find(unit => unit.symbolName === 'inner').parentSymbol, 'Box.run');
  assert.equal(result.units.find(unit => unit.symbolName === 'read').qualifiedName, 'object.read');
  assert.equal(result.units.find(unit => unit.symbolName === 'nested').qualifiedName, 'object.nested');
  assert.ok(result.units.some(unit => unit.symbolName === 'Empty' && unit.kind === 'class'));
  assertRanges(text, result);
});

test('TypeScript interfaces, namespace classes and TSX components use their actual grammars', async () => {
  const typescript = 'interface Reader<T> { read(value:T): T; value:T; }\n' +
    'export namespace Library { export class Box<T> { run<U>(value:U):U { return value; } } }\ntype Alias = { name: string };';
  const result = await parseCodeStructure(source('sample.ts', typescript));
  assert.equal(result.parseStatus, 'parsed');
  assert.equal(result.units.find(unit => unit.symbolName === 'read').qualifiedName, 'Reader.read');
  assert.equal(result.units.find(unit => unit.symbolName === 'run').qualifiedName, 'Library.Box.run');
  assert.ok(result.units.some(unit => unit.kind === 'type' && unit.symbolName === 'Alias'));
  assertRanges(typescript, result);
  const tsx = '// 😀\r\nexport const View = (props: {name:string}) => <main>{props.name}</main>;';
  const view = await parseCodeStructure(source('view.tsx', tsx));
  assert.equal(view.language, 'tsx');
  assert.equal(view.parseStatus, 'parsed');
  assert.deepEqual(view.units.map(unit => unit.symbolName), ['View'], 'parameter type fields cannot swallow the component body');
  assert.match(tsx.slice(view.units[0].startOffset, view.units[0].endOffset), /^View\s*=/);
  assertRanges(tsx, view);
});

test('unsupported sources stay explicit and chat or memory titles cannot turn into executable source identity', async () => {
  for (const extension of ['.js', '.mjs', '.cjs', '.jsx']) assert.equal(codeLanguageForSource(source(`file${extension}`, '')), 'javascript');
  for (const extension of ['.ts', '.mts', '.cts']) assert.equal(codeLanguageForSource(source(`file${extension}`, '')), 'typescript');
  assert.equal(codeLanguageForSource(source('file.CS', '')), 'csharp');
  assert.equal(codeLanguageForSource({ text: '', title: 'file.tsx' }), 'tsx');
  assert.equal(codeLanguageForSource(source('notes.cs', '', { sourceType: 'message' })), null);
  assert.equal(codeLanguageForSource(source('notes.ts', '', { sourceType: 'memory' })), null);
  const result = await parseCodeStructure(source('unsupported.py', 'def ignored(): pass'));
  assert.equal(result.parseStatus, 'unavailable');
  assert.deepEqual(result.diagnosticCodes, ['CODE_LANGUAGE_UNSUPPORTED']);
  assert.deepEqual(result.units, []);
  await assert.rejects(parseCodeStructure({ title: 'invalid.js' }), { code: 'INVALID_CODE_SOURCE' });
});

test('TypeScript overload signatures and declaration-only class methods stay separate from implementations', async () => {
  const text = 'class Reader { read(value:string):string; read(value:number):number; read(value:any){return value;} }\n' +
    'function fetch(value:string):string; function fetch(value:number):number; function fetch(value:any){return value;}\n' +
    'declare class Declared { constructor(value:string); read(value:string):string; }';
  const result = await parseCodeStructure(source('overloads.ts', text));
  assert.equal(result.parseStatus, 'parsed');
  assert.equal(result.units.filter(unit => unit.qualifiedName === 'Reader.read').length, 3);
  assert.equal(result.units.filter(unit => unit.qualifiedName === 'fetch').length, 3);
  assert.ok(result.units.some(unit => unit.kind === 'constructor' && unit.parentSymbol === 'Declared'));
  assert.ok(result.units.some(unit => unit.qualifiedName === 'Declared.read'));
  assertRanges(text, result);
});

test('outer function fragments remain searchable around nested named declarations with complete readback bounds', async () => {
  const text = '// 😀\r\nexport function collectLexicalTerms(text) {\r\n' +
    '  const append = (term) => term.toLowerCase();\r\n' +
    '  function normalize(value) { return append(value); }\r\n' +
    '  return normalize(text);\r\n}';
  const result = await parseCodeStructure(source('lexical.mjs', text));
  assert.equal(result.parseStatus, 'parsed');
  const outer = result.units.filter(unit => unit.qualifiedName === 'collectLexicalTerms');
  assert.equal(outer.length, 3);
  assert.match(text.slice(outer[0].startOffset, outer[0].endOffset), /^function collectLexicalTerms/);
  assert.ok(outer.some(unit => text.slice(unit.startOffset, unit.endOffset).includes('return normalize(text)')));
  for (const unit of outer) {
    assert.equal(text.slice(unit.unitStartOffset, unit.unitEndOffset), text.slice(text.indexOf('function collectLexicalTerms')));
  }
  assert.equal(result.units.find(unit => unit.symbolName === 'append').parentSymbol, 'collectLexicalTerms');
  assert.equal(result.units.find(unit => unit.symbolName === 'normalize').parentSymbol, 'collectLexicalTerms');
  assertRanges(text, result);
});

test('nested TypeScript classes and functions preserve all owners across shared fragment boundaries', async () => {
  const text = 'class Outer<T> {\r\n  build() {\r\n' +
    '    const create = () => class Inner { run() { return "😀"; } };\r\n' +
    '    return create();\r\n  }\r\n}';
  const result = await parseCodeStructure(source('nested.ts', text));
  assert.equal(result.parseStatus, 'parsed');
  for (const qualifiedName of ['Outer', 'Outer.build', 'Outer.build.create',
    'Outer.build.create.Inner', 'Outer.build.create.Inner.run']) {
    assert.ok(result.units.some(unit => unit.qualifiedName === qualifiedName), `${qualifiedName} keeps an assigned fragment`);
  }
  const inner = result.units.find(unit => unit.qualifiedName === 'Outer.build.create.Inner');
  const method = result.units.find(unit => unit.qualifiedName === 'Outer.build.create.Inner.run');
  assert.equal(inner.endOffset, method.startOffset, 'parent header ends at the child declaration boundary');
  assert.equal(method.parentSymbol, 'Outer.build.create.Inner');
  assert.match(text.slice(inner.unitStartOffset, inner.unitEndOffset), /^class Inner \{ run\(\)/);
  assertRanges(text, result);
});

test('computed method expressions retain source ranges without inventing symbols or invalid labels', async () => {
  for (const text of ['class A { [\n  "read"\n]() { function child() { return 1; } return child(); } safe() { return 2; } }',
    'const object = { [\n "key"\n](value) { return value; }, safe() { return 3; } };']) {
    const input = source('computed.js', text), result = await parseCodeStructure(input);
    assert.equal(result.parseStatus, 'partial');
    assert.ok(result.diagnosticCodes.includes('CODE_STRUCTURE_DYNAMIC_SYMBOL'));
    assert.ok(result.units.some(unit => unit.kind === 'method' && unit.symbolName === undefined));
    assert.ok(result.units.some(unit => unit.symbolName === 'safe'));
    for (const unit of result.units) for (const key of ['symbolName', 'qualifiedName', 'parentSymbol'])
      if (unit[key] !== undefined) assert.doesNotMatch(unit[key], /[\x00-\x1f]|\[/u);
    const child = result.units.find(unit => unit.symbolName === 'child');
    if (child) { assert.equal(child.parentSymbol, undefined); assert.equal(child.qualifiedName, undefined); }
    const chunks = chunkStructuredSource(input, result);
    assert.ok(chunks.some(chunk => chunk.text.includes('return')));
    assertRanges(text, result);
  }
  const input = source('namespace.cs', 'namespace Kynxa.\n Core; class Job { public int Read() => 1; }');
  const result = await parseCodeStructure(input);
  assert.equal(result.parseStatus, 'partial');
  assert.ok(result.diagnosticCodes.includes('CODE_STRUCTURE_DYNAMIC_SYMBOL'));
  assert.ok(result.units.some(unit => unit.symbolName === 'Read'));
  assert.ok(chunkStructuredSource(input, result).some(chunk => chunk.structure.symbolName === 'Read'));
});

test('syntax errors are reported as partial evidence while independently valid declarations remain usable', async () => {
  const text = 'function valid() { return 1; }\nfunction broken( { ???';
  const result = await parseCodeStructure(source('broken.js', text));
  assert.equal(result.parseStatus, 'partial');
  assert.ok(result.diagnosticCodes.includes('CODE_SYNTAX_ERROR'));
  assert.ok(result.units.some(unit => unit.symbolName === 'valid'));
  assertRanges(text, result);
});

test('native progress cancellation disposes owned resources and never resumes into a later file', async () => {
  const { Parser, Tree } = require('@vscode/tree-sitter-wasm');
  const originalParse = Parser.prototype.parse, originalDeleteParser = Parser.prototype.delete, originalDeleteTree = Tree.prototype.delete;
  const controller = new AbortController();
  let callbacks = 0, parsersDeleted = 0, treesDeleted = 0;
  Parser.prototype.parse = function (text, tree, options) {
    return originalParse.call(this, text, tree, { ...options, progressCallback: state => {
      if (++callbacks >= 3) controller.abort();
      return options.progressCallback(state);
    } });
  };
  Parser.prototype.delete = function () { parsersDeleted++; return originalDeleteParser.call(this); };
  Tree.prototype.delete = function () { treesDeleted++; return originalDeleteTree.call(this); };
  try {
    const text = Array.from({ length: 3000 }, (_, index) => `function f${index}() { return ${index}; }`).join('\n');
    await assert.rejects(parseCodeStructure(source('cancelled.js', text), { signal: controller.signal }), { name: 'AbortError' });
    assert.ok(callbacks >= 3, 'the real native parser invoked its configured cancellation callback');
    assert.equal(parsersDeleted, 1);
    assert.ok(treesDeleted <= 1, 'cancelled native parsing may return no tree');
  } finally {
    Parser.prototype.parse = originalParse;
    Parser.prototype.delete = originalDeleteParser;
    Tree.prototype.delete = originalDeleteTree;
  }
  const next = await parseCodeStructure(source('later.js', 'function next() { return 2; }'));
  assert.equal(next.parseStatus, 'parsed');
  assert.deepEqual(next.units.map(unit => unit.symbolName), ['next']);
});

test('pre-cancellation and traversal cancellation reject without publishing a partial success', async () => {
  const before = new AbortController(); before.abort();
  await assert.rejects(parseCodeStructure(source('before.js', 'function before() {}'), { signal: before.signal }), { name: 'AbortError' });
  const during = new AbortController();
  const text = Array.from({ length: 2000 }, (_, index) => `function f${index}() { return ${index}; }`).join('\n');
  const parsing = parseCodeStructure(source('during.js', text), { signal: during.signal });
  const timer = setTimeout(() => during.abort(), 0);
  try { await assert.rejects(parsing, { name: 'AbortError' }); }
  finally { clearTimeout(timer); }
});

test('very large declaration counts return explicit bounded coverage instead of silently dropping code', async () => {
  const text = Array.from({ length: 12000 }, (_, index) => `function f${index}() { return ${index}; }`).join('\n');
  const result = await parseCodeStructure(source('large.js', text));
  assert.notEqual(result.parseStatus, 'parsed');
  assert.ok(result.diagnosticCodes.some(code => ['CODE_STRUCTURE_LIMIT', 'CODE_PARSER_TIMEOUT'].includes(code)));
  assert.ok(result.units.length <= 5000);
  assertRanges(text, result);
});
