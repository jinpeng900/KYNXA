import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import { dirname, extname, join } from 'node:path';
import { MAX_SOURCE_CHARACTERS, retrievalFailure } from './retrieval-contracts.mjs';

const require = createRequire(import.meta.url);
const PACKAGE_VERSION = '0.3.1';
export const CODE_PARSER_VERSION = `vscode-tree-sitter-wasm-${PACKAGE_VERSION}:units-v3`;
const MAX_VISITED_NODES = 100_000;
const MAX_DECLARATIONS = 10_000;
const MAX_UNITS = 5000;
const MAX_PARSE_TIME_MS = 250;
const MAX_TRAVERSAL_TIME_MS = 250;
const MAX_SYMBOL_CHARACTERS = 256;
const MAX_QUALIFIED_CHARACTERS = 512;
const GRAMMARS = Object.freeze({ csharp: 'c-sharp', javascript: 'javascript', typescript: 'typescript', tsx: 'tsx' });
const EXTENSIONS = new Map([['.cs', 'csharp'], ['.js', 'javascript'], ['.mjs', 'javascript'], ['.cjs', 'javascript'],
  ['.jsx', 'javascript'], ['.ts', 'typescript'], ['.mts', 'typescript'], ['.cts', 'typescript'], ['.tsx', 'tsx']]);
const TYPE_KINDS = new Map([['class_declaration', 'class'], ['class', 'class'], ['interface_declaration', 'interface'],
  ['struct_declaration', 'struct'], ['record_declaration', 'record'], ['enum_declaration', 'enum'], ['type_alias_declaration', 'type']]);
const CALLABLE_KINDS = new Map([['function_declaration', 'function'], ['generator_function_declaration', 'function'],
  ['function_signature', 'function'],
  ['function_expression', 'function'], ['generator_function', 'function'], ['arrow_function', 'function'],
  ['lambda_expression', 'function'], ['local_function_statement', 'function'], ['method_declaration', 'method'],
  ['method_definition', 'method'], ['method_signature', 'method'], ['abstract_method_signature', 'method'],
  ['constructor_declaration', 'constructor'], ['destructor_declaration', 'method'],
  ['property_declaration', 'property'], ['property_signature', 'property']]);
const BINDING_PARENTS = new Set(['variable_declarator', 'pair', 'field_definition', 'public_field_definition']);
const ANONYMOUS_CALLABLES = new Set(['function_expression', 'generator_function', 'arrow_function', 'lambda_expression']);
const NAMESPACES = new Set(['namespace_declaration', 'internal_module', 'module']);
const languages = new Map();
let runtimePromise;

export function codeLanguageForSource(source) {
  if (['message', 'memory'].includes(source?.sourceType)) return null;
  const filename = [source?.locator?.relativePath, source?.locator?.path, source?.title]
    .find(value => typeof value === 'string' && value.trim());
  return typeof filename === 'string' ? EXTENSIONS.get(extname(filename).toLowerCase()) ?? null : null;
}

function checkCancelled(signal) {
  if (signal?.aborted) throw Object.assign(new Error('Code parsing was cancelled. / 代码解析已取消。'),
    { name: 'AbortError', code: 'ABORT_ERR' });
}

async function runtime() {
  if (!runtimePromise) runtimePromise = Promise.resolve().then(async () => {
    if (require('@vscode/tree-sitter-wasm/package.json').version !== PACKAGE_VERSION)
      throw retrievalFailure('Unsupported packaged parser version. / 随包解析器版本不受支持。', 'CODE_PARSER_VERSION_UNSUPPORTED');
    const api = require('@vscode/tree-sitter-wasm');
    const wasmRoot = dirname(require.resolve('@vscode/tree-sitter-wasm'));
    await api.Parser.init({ locateFile: filename => join(wasmRoot, filename) });
    return { ...api, wasmRoot };
  }).catch(error => { runtimePromise = undefined; throw error; });
  return runtimePromise;
}

async function languageFor(language) {
  let pending = languages.get(language);
  if (!pending) {
    pending = runtime().then(async api => {
      // Buffer-backed loading uses only the fixed packaged assets, never URL fetching or a compiler.
      // 只从固定随包资产读取字节加载语法，不进行 URL 获取或调用编译器。
      const bytes = await readFile(join(api.wasmRoot, `tree-sitter-${GRAMMARS[language]}.wasm`));
      return { Parser: api.Parser, language: await api.Language.load(bytes) };
    }).catch(error => { languages.delete(language); throw error; });
    languages.set(language, pending);
  }
  return pending;
}

function bindingName(node) {
  const parent = node.parent;
  if (!parent || !BINDING_PARENTS.has(parent.type) || parent.childForFieldName('value')?.id !== node.id) return null;
  return parent.childForFieldName(parent.type === 'pair' ? 'key' : 'name');
}

function declaration(node) {
  if (!node.isNamed) return null;
  if (node.type === 'property_signature' && node.parent?.type !== 'interface_body') return null;
  if (node.type === 'method_signature' && !['interface_body', 'class_body'].includes(node.parent?.type)) return null;
  const isNamespace = NAMESPACES.has(node.type);
  const isObject = node.type === 'object';
  const kind = TYPE_KINDS.get(node.type) ?? CALLABLE_KINDS.get(node.type);
  if (!kind && !isNamespace && !isObject) return null;
  const binding = bindingName(node);
  const nameNode = node.childForFieldName('name') ?? binding;
  if (ANONYMOUS_CALLABLES.has(node.type) && !nameNode) return null;
  const declaredName = nameNode?.text ?? null;
  // Computed keys are runtime expressions, not stable declarations; never invent a searchable symbol.
  // 计算属性键是运行时表达式，不能冒称稳定声明；控制字符名称也不能进入索引标签。
  const unsupportedName = nameNode?.type === 'computed_property_name' ||
    nameNode?.type === 'object_pattern' || nameNode?.type === 'array_pattern' ||
    typeof declaredName === 'string' && /[\x00-\x1f]/u.test(declaredName);
  const symbolName = unsupportedName ? null : declaredName;
  const startNode = binding && ANONYMOUS_CALLABLES.has(node.type) ? node.parent : node;
  const constructor = ['method_definition', 'method_signature'].includes(node.type) && symbolName === 'constructor' && node.parent?.type === 'class_body';
  return { kind: constructor ? 'constructor' : kind, symbolName, startNode, unsupportedName };
}

function nestedScope(parent, symbolName, diagnostics) {
  if (!symbolName || parent.overflow) return { qualifiedName: undefined, overflow: true };
  if (/[\x00-\x1f]/u.test(symbolName)) {
    diagnostics.add('CODE_STRUCTURE_DYNAMIC_SYMBOL');
    return { qualifiedName: undefined, overflow: true };
  }
  const qualifiedName = parent.qualifiedName ? `${parent.qualifiedName}.${symbolName}` : symbolName;
  if (qualifiedName.length > MAX_QUALIFIED_CHARACTERS) {
    diagnostics.add('CODE_STRUCTURE_SYMBOL_LIMIT');
    return { qualifiedName: undefined, overflow: true };
  }
  return { qualifiedName, overflow: false };
}

function fileNamespace(root, language) {
  if (language !== 'csharp') return null;
  const cursor = root.walk();
  let visited = 0;
  try {
    if (!cursor.gotoFirstChild()) return null;
    do {
      const node = cursor.currentNode;
      if (node.type === 'file_scoped_namespace_declaration') return node.childForFieldName('name')?.text ?? null;
      if (TYPE_KINDS.has(node.type) || node.type === 'namespace_declaration' || ++visited >= MAX_VISITED_NODES) return null;
    } while (cursor.gotoNextSibling());
    return null;
  } finally { cursor.delete(); }
}

function isValidRange(text, start, end) {
  return Number.isSafeInteger(start) && Number.isSafeInteger(end) && start >= 0 && end > start && end <= text.length &&
    !(/[\uD800-\uDBFF]/u.test(text[start - 1] ?? '') && /[\uDC00-\uDFFF]/u.test(text[start] ?? '')) &&
    !(/[\uD800-\uDBFF]/u.test(text[end - 1] ?? '') && /[\uDC00-\uDFFF]/u.test(text[end] ?? ''));
}

async function collectUnits(tree, text, language, signal, diagnostics) {
  const declarations = [], frames = [];
  const namespace = fileNamespace(tree.rootNode, language);
  const initialScope = namespace ? nestedScope({ qualifiedName: undefined, overflow: false }, namespace, diagnostics)
    : { qualifiedName: undefined, overflow: false };
  const cursor = tree.walk();
  const deadline = performance.now() + MAX_TRAVERSAL_TIME_MS;
  let visitedNodes = 0;
  try {
    while (true) {
      checkCancelled(signal);
      if (++visitedNodes > MAX_VISITED_NODES || declarations.length >= MAX_DECLARATIONS || performance.now() > deadline) {
        diagnostics.add('CODE_STRUCTURE_LIMIT');
        break;
      }
      if (visitedNodes % 256 === 0) {
        await new Promise(ready => setImmediate(ready));
        checkCancelled(signal);
      }
      const node = cursor.currentNode;
      const parent = frames.at(-1) ?? { scope: initialScope };
      let scope = parent.scope;
      const info = declaration(node);
      if (info) {
        if (info.unsupportedName) diagnostics.add('CODE_STRUCTURE_DYNAMIC_SYMBOL');
        scope = nestedScope(parent.scope, info.symbolName, diagnostics);
        const symbolName = info.symbolName && info.symbolName.length <= MAX_SYMBOL_CHARACTERS ? info.symbolName : undefined;
        if (info.symbolName && !symbolName) diagnostics.add('CODE_STRUCTURE_SYMBOL_LIMIT');
        if (info.kind && !node.hasError && !node.isMissing) {
          const start = info.startNode.startIndex, end = node.endIndex;
          if (isValidRange(text, start, end)) {
            const unit = { kind: info.kind, ...(symbolName ? { symbolName } : {}),
              ...(scope.qualifiedName ? { qualifiedName: scope.qualifiedName } : {}),
              ...(parent.scope.qualifiedName ? { parentSymbol: parent.scope.qualifiedName } : {}),
              startOffset: start, endOffset: end, startLine: info.startNode.startPosition.row + 1, endLine: node.endPosition.row + 1 };
            declarations.push({ unit, depth: frames.length });
          } else diagnostics.add('CODE_STRUCTURE_INVALID_RANGE');
        }
      }
      frames.push({ scope });
      if (cursor.gotoFirstChild()) continue;
      frames.pop();
      while (!cursor.gotoNextSibling()) {
        if (!cursor.gotoParent()) return selectedUnits(declarations, text, signal, diagnostics);
        frames.pop();
      }
    }
    return selectedUnits(declarations, text, signal, diagnostics);
  } finally { cursor.delete(); }
}

async function selectedUnits(declarations, text, signal, diagnostics) {
  // The deepest declaration owns each span; parent headers/tails keep their symbols and full ranges.
  // 最深声明拥有当前片段；父声明的头尾仍保留自身符号及完整范围，避免整类吞掉方法。
  const events = declarations.flatMap(({ unit }, owner) => [
    { offset: unit.startOffset, owner, active: true }, { offset: unit.endOffset, owner, active: false }
  ]).sort((left, right) => left.offset - right.offset || Number(left.active) - Number(right.active));
  const active = new Uint8Array(declarations.length), heap = [], units = [];
  const deadline = performance.now() + MAX_TRAVERSAL_TIME_MS;
  let previousOffset = events[0]?.offset ?? 0, fragment, scannedOffset = 0, line = 1;
  const above = (left, right) => declarations[left].depth > declarations[right].depth ||
    (declarations[left].depth === declarations[right].depth && left > right);
  function activate(owner) {
    active[owner] = 1;
    heap.push(owner);
    let child = heap.length - 1;
    while (child > 0) {
      const parent = Math.floor((child - 1) / 2);
      if (!above(heap[child], heap[parent])) break;
      [heap[child], heap[parent]] = [heap[parent], heap[child]];
      child = parent;
    }
  }
  function currentOwner() {
    while (heap.length && !active[heap[0]]) {
      const last = heap.pop();
      if (!heap.length) break;
      heap[0] = last;
      let parent = 0;
      while (true) {
        let child = parent * 2 + 1;
        if (child >= heap.length) break;
        if (child + 1 < heap.length && above(heap[child + 1], heap[child])) child++;
        if (!above(heap[child], heap[parent])) break;
        [heap[parent], heap[child]] = [heap[child], heap[parent]];
        parent = child;
      }
    }
    return heap[0];
  }
  function finishFragment() {
    if (!fragment) return true;
    const { owner, startOffset, endOffset } = fragment;
    fragment = undefined;
    if (!text.slice(startOffset, endOffset).trim()) return true;
    if (units.length >= MAX_UNITS) { diagnostics.add('CODE_STRUCTURE_LIMIT'); return false; }
    for (; scannedOffset < startOffset; scannedOffset++) if (text[scannedOffset] === '\n') line++;
    const startLine = line;
    for (; scannedOffset < endOffset; scannedOffset++) if (text[scannedOffset] === '\n') line++;
    const declaration = declarations[owner].unit;
    units.push({ ...declaration, unitStartOffset: declaration.startOffset, unitEndOffset: declaration.endOffset,
      startOffset, endOffset, startLine, endLine: line });
    return true;
  }
  for (let index = 0, groups = 0; index < events.length; groups++) {
    checkCancelled(signal);
    if (performance.now() > deadline) { diagnostics.add('CODE_STRUCTURE_LIMIT'); break; }
    if (groups && groups % 256 === 0) {
      await new Promise(ready => setImmediate(ready));
      checkCancelled(signal);
    }
    const offset = events[index].offset, owner = currentOwner();
    if (offset > previousOffset) {
      if (owner !== undefined) {
        if (fragment?.owner === owner && fragment.endOffset === previousOffset) fragment.endOffset = offset;
        else {
          if (!finishFragment()) return units;
          fragment = { owner, startOffset: previousOffset, endOffset: offset };
        }
      } else if (!finishFragment()) return units;
    }
    while (index < events.length && events[index].offset === offset) {
      const event = events[index++];
      if (event.active) activate(event.owner);
      else active[event.owner] = 0;
    }
    previousOffset = offset;
  }
  finishFragment();
  return units;
}

/** Parse supplied text with owned per-call parser/tree resources; evidence authorization remains upstream.
 * 解析调用方提供的文本，每次调用拥有 parser/tree 并负责释放；证据授权仍由上层负责。 */
export async function parseCodeStructure(source, { signal } = {}) {
  checkCancelled(signal);
  if (typeof source?.text !== 'string' || source.text.length > MAX_SOURCE_CHARACTERS)
    throw retrievalFailure('Code source text is invalid or too large. / 代码原文无效或过大。', 'INVALID_CODE_SOURCE');
  const language = codeLanguageForSource(source);
  const result = { domain: 'code', language, parserVersion: `${CODE_PARSER_VERSION}:${language ?? 'unsupported'}`,
    parseStatus: 'unavailable', diagnosticCodes: [], units: [] };
  if (!language) return { ...result, diagnosticCodes: ['CODE_LANGUAGE_UNSUPPORTED'] };
  let parser, tree;
  try {
    const loaded = await languageFor(language);
    checkCancelled(signal);
    parser = new loaded.Parser();
    parser.setLanguage(loaded.language);
    parser.setTimeoutMicros(MAX_PARSE_TIME_MS * 1000);
    const deadline = performance.now() + MAX_PARSE_TIME_MS;
    tree = parser.parse(source.text, null, { progressCallback: () => signal?.aborted || performance.now() > deadline });
    checkCancelled(signal);
    if (!tree) return { ...result, diagnosticCodes: ['CODE_PARSER_TIMEOUT'] };
    const diagnostics = new Set(tree.rootNode.hasError ? ['CODE_SYNTAX_ERROR'] : []);
    const units = await collectUnits(tree, source.text, language, signal, diagnostics);
    checkCancelled(signal);
    return { ...result, units, parseStatus: diagnostics.size ? 'partial' : 'parsed', diagnosticCodes: [...diagnostics] };
  } catch (error) {
    checkCancelled(signal);
    return { ...result, diagnosticCodes: [error.code === 'ENOENT' ? 'CODE_PARSER_ASSET_MISSING'
      : error.code === 'CODE_PARSER_VERSION_UNSUPPORTED' ? error.code : 'CODE_PARSER_UNAVAILABLE'] };
  } finally {
    try { tree?.delete(); } finally { parser?.delete(); }
  }
}
