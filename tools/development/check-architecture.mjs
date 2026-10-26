import { readFile, readdir, stat } from 'node:fs/promises';
import { dirname, extname, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkRustArchitecture } from './check-rust-architecture.mjs';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const manifest = JSON.parse(await readFile(resolve(repositoryRoot, 'tools/development/architecture-rules.json'), 'utf8'));
const violations = [];
const toRepositoryPath = path => relative(repositoryRoot, path).split(sep).join('/');
const matchesRule = (path, rule) => (!rule.extensions || rule.extensions.includes(extname(path))) &&
  (rule.files?.includes(path) || (rule.prefix && path.startsWith(rule.prefix)) ||
    (rule.directory && path.slice(0, path.lastIndexOf('/')) === rule.directory));
const componentsOf = path => manifest.componentRules.filter(rule => matchesRule(path, rule)).map(rule => rule.component);

async function listFiles(directory, excludedNames = [], includedDirectories = []) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    // Native src/bin is source, while .NET bin remains generated output.
    // 原生 src/bin 属于源码；.NET bin 仍然属于生成产物，例外只按清单中的精确目录生效。
    if (excludedNames.includes(entry.name) && !includedDirectories.includes(toRepositoryPath(path))) continue;
    if (entry.isDirectory()) files.push(...await listFiles(path, excludedNames, includedDirectories));
    else if (entry.isFile()) files.push(path);
  }
  return files.sort();
}

// Ignore strings, comments and templates when finding imports.
// 查找导入时跳过字符串、注释和模板。
// This limited scanner does not resolve computed references.
// 此扫描器只处理当前静态语法，不解析动态计算的引用。
function codeTokens(source) {
  const tokens = [];
  let offset = 0;
  function skipQuoted(quote) {
    const start = offset++;
    while (offset < source.length) {
      if (source[offset] === '\\') { offset += 2; continue; }
      if (source[offset++] === quote) break;
    }
    return source.slice(start + 1, offset - 1);
  }
  function skipComment() {
    if (source.startsWith('//', offset)) {
      while (offset < source.length && source[offset] !== '\n') offset++;
    } else {
      const end = source.indexOf('*/', offset + 2);
      offset = end < 0 ? source.length : end + 2;
    }
  }
  function skipTemplate() {
    offset++;
    while (offset < source.length) {
      if (source[offset] === '\\') { offset += 2; continue; }
      if (source[offset] === '`') { offset++; return; }
      if (source.startsWith('${', offset)) {
        offset += 2;
        let braceDepth = 1;
        while (offset < source.length && braceDepth) {
          if (source[offset] === '"' || source[offset] === "'") skipQuoted(source[offset]);
          else if (source[offset] === '`') skipTemplate();
          else if (source.startsWith('//', offset) || source.startsWith('/*', offset)) skipComment();
          else {
            if (source[offset] === '{') braceDepth++;
            if (source[offset] === '}') braceDepth--;
            offset++;
          }
        }
      } else offset++;
    }
  }
  while (offset < source.length) {
    const character = source[offset];
    if (/\s/.test(character)) { offset++; continue; }
    if (source.startsWith('//', offset) || source.startsWith('/*', offset)) { skipComment(); continue; }
    if (character === '"' || character === "'") {
      tokens.push({ kind: 'string', value: skipQuoted(character) });
      continue;
    }
    if (character === '`') { skipTemplate(); tokens.push({ kind: 'template', value: '' }); continue; }
    const previous = tokens.at(-1)?.value;
    if (character === '/' && (!previous || ['=', '(', '[', ',', ':', '!', 'return', '=>', '&&', '||', '?'].includes(previous))) {
      offset++;
      let inCharacterClass = false;
      while (offset < source.length) {
        if (source[offset] === '\\') { offset += 2; continue; }
        if (source[offset] === '[') inCharacterClass = true;
        if (source[offset] === ']') inCharacterClass = false;
        if (source[offset++] === '/' && !inCharacterClass) break;
      }
      while (/[a-z]/i.test(source[offset] ?? '') && offset < source.length) offset++;
      tokens.push({ kind: 'regex', value: '' });
      continue;
    }
    const identifier = source.slice(offset).match(/^[A-Za-z_$][\w$]*/);
    if (identifier) {
      tokens.push({ kind: 'identifier', value: identifier[0] });
      offset += identifier[0].length;
    } else {
      const pair = source.slice(offset, offset + 2);
      const value = ['=>', '&&', '||'].includes(pair) ? pair : character;
      tokens.push({ kind: 'punctuation', value });
      offset += value.length;
    }
  }
  return tokens;
}

function staticReferences(source) {
  const tokens = codeTokens(source);
  const references = [];
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index];
    if (token.kind !== 'identifier') continue;
    if (token.value === 'import' && tokens[index + 1]?.kind === 'string') {
      references.push({ kind: 'module', specifier: tokens[index + 1].value });
    } else if ((token.value === 'import' && !['(', '.'].includes(tokens[index + 1]?.value)) ||
        (token.value === 'export' && ['{', '*'].includes(tokens[index + 1]?.value))) {
      for (let cursor = index + 1; cursor < tokens.length && tokens[cursor].value !== ';'; cursor++) {
        if (tokens[cursor].value === 'from' && tokens[cursor + 1]?.kind === 'string') {
          references.push({ kind: 'module', specifier: tokens[cursor + 1].value });
          break;
        }
      }
    }
    if (token.value === 'new' && tokens[index + 1]?.value === 'URL' && tokens[index + 2]?.value === '(' &&
        tokens[index + 3]?.kind === 'string' && tokens.slice(index + 4, index + 11).map(value => value.value).join('') === ',import.meta.url)') {
      references.push({ kind: 'resource', specifier: tokens[index + 3].value });
    }
  }
  return references.filter(reference => /^\.\.?\//.test(reference.specifier));
}

const sourceFiles = [];
const localTestFiles = new Set();
for (const path of manifest.localTestFiles ?? []) {
  if (path.includes('\\') || path.split('/').includes('..') || !path.endsWith('_tests.rs') ||
      !manifest.sourceScopes.some(scope => path.startsWith(`${scope}/`)) || localTestFiles.has(path)) {
    violations.push({ code: 'INVALID_LOCAL_TEST_FILE', source: path });
    continue;
  }
  localTestFiles.add(path);
}
for (const scope of manifest.sourceScopes) {
  if (!scope.startsWith('apps/')) throw new Error(`Source scope must be inside apps: ${scope}`);
  sourceFiles.push(...(await listFiles(resolve(repositoryRoot, scope), manifest.excludedDirectoryNames, manifest.includedSourceDirectories ?? []))
    .filter(path => !localTestFiles.has(toRepositoryPath(path)) &&
      (manifest.sourceExtensions.includes(extname(path)) || (manifest.resourceScopes ?? []).some(resource =>
        toRepositoryPath(path).startsWith(`${resource.root}/`) && resource.extensions.includes(extname(path))))));
}
const componentCounts = Object.fromEntries(Object.keys(manifest.components).map(component => [component, 0]));
for (const path of sourceFiles) {
  const source = toRepositoryPath(path);
  const components = componentsOf(source);
  if (components.length !== 1 || !Object.hasOwn(manifest.components, components[0])) violations.push({ code: 'SOURCE_COMPONENT', source, components });
  else componentCounts[components[0]]++;
}

const scriptFiles = sourceFiles.filter(path => ['.mjs', '.js'].includes(extname(path)));
let relativeModuleCount = 0;
let relativeResourceCount = 0;
for (const path of scriptFiles) {
  const source = toRepositoryPath(path);
  const references = staticReferences(await readFile(path, 'utf8'));
  for (const reference of references) {
    if (reference.kind === 'module') relativeModuleCount++;
    else relativeResourceCount++;
    const target = resolve(dirname(path), reference.specifier);
    let exists = false;
    try {
      const targetStatus = await stat(target);
      exists = reference.kind === 'resource' || targetStatus.isFile();
    } catch (error) { if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error; }
    if (!exists) violations.push({ code: 'MISSING_RELATIVE_TARGET', source, ...reference, target: toRepositoryPath(target) });
  }
}

// Test suites are described by their direct implementation dependencies.
// 测试套件按直接实现依赖统计技术组件。
const testSuites = new Map();
const missingLocalTestRoots = [];
let testSourceCount = 0;
for (const testRoot of manifest.localTests.roots) {
  try { await stat(resolve(repositoryRoot, testRoot)); }
  catch (error) {
    // Local test files are optional; missing production scopes still fail above.
    // 本地测试可不提供；前面的生产目录缺失仍然报错。
    if (!manifest.localTests.localOnly || error.code !== 'ENOENT') throw error;
    missingLocalTestRoots.push(testRoot);
    continue;
  }
  const testFiles = await listFiles(resolve(repositoryRoot, testRoot), ['node_modules', 'bin', 'obj', '.git']);
  for (const path of testFiles) {
    if (!['.mjs', '.csproj'].includes(extname(path))) continue;
    testSourceCount++;
    const suite = `${testRoot}/${relative(resolve(repositoryRoot, testRoot), path).split(sep)[0]}`;
    const components = testSuites.get(suite) ?? new Set();
    const source = await readFile(path, 'utf8');
    const targets = extname(path) === '.mjs' ? staticReferences(source).filter(reference => reference.kind === 'module').map(reference => reference.specifier) :
      [...source.matchAll(/<Compile\s+Include="([^"]+)"/g)].map(match => match[1].replaceAll('\\', '/'));
    for (const target of targets) {
      for (const component of componentsOf(toRepositoryPath(resolve(dirname(path), target)))) components.add(component);
    }
    testSuites.set(suite, components);
  }
}
const testDependencies = [...testSuites].sort(([first], [second]) => first.localeCompare(second)).map(([suite, components]) => ({
  suite, components: components.size ? [...components].sort() : [manifest.localTests.fallbackComponent],
  basis: components.size ? 'direct-production-dependency' : 'integration'
}));
const report = {
  passed: violations.length === 0,
  productionSourceFiles: sourceFiles.length,
  sourceFilesByComponent: componentCounts,
  scriptModuleFiles: scriptFiles.length,
  relativeModuleReferences: relativeModuleCount,
  relativeResourceReferences: relativeResourceCount,
  testSourceOrProjectFiles: testSourceCount,
  missingLocalTestRoots,
  testSuites: testDependencies.length,
  testSuitesByComponent: Object.fromEntries(Object.keys(manifest.components).map(component =>
    [component, testDependencies.filter(dependency => dependency.components.includes(component)).length])),
  detailsCommand: 'node tools/development/check-architecture.mjs --details',
  ...(process.argv.includes('--details') ? { testDependencies } : {}),
  coverageNotes: manifest.coverageNotes,
  violations
};
const rust = await checkRustArchitecture({ repositoryRoot, manifest, sourceFiles });
violations.push(...rust.violations);
report.rust = { ...rust, violations: undefined };
report.passed = violations.length === 0;
console.log(JSON.stringify(report, null, 2));
process.exitCode = violations.length ? 1 : 0;
