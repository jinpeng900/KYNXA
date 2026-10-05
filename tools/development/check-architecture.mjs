import { readFile, readdir, stat } from 'node:fs/promises';
import { dirname, extname, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const manifest = JSON.parse(await readFile(resolve(repositoryRoot, 'docs/team/module-ownership.json'), 'utf8'));
const violations = [];
const toRepositoryPath = path => relative(repositoryRoot, path).split(sep).join('/');
const matchesRule = (path, rule) => (!rule.extensions || rule.extensions.includes(extname(path))) &&
  (rule.files?.includes(path) || (rule.prefix && path.startsWith(rule.prefix)) ||
    (rule.directory && path.slice(0, path.lastIndexOf('/')) === rule.directory));
const ownersOf = path => manifest.ownershipRules.filter(rule => matchesRule(path, rule)).map(rule => rule.role);

async function listFiles(directory, excludedNames = []) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (excludedNames.includes(entry.name)) continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) files.push(...await listFiles(path, excludedNames));
    else if (entry.isFile()) files.push(path);
  }
  return files.sort();
}

// Read only code tokens: quoted prompts, comments and complete templates cannot become imports.
// 只读取代码 token：引号中的提示词、注释和完整模板不得被识别为导入。
// This is a bounded lexer for current syntax, not a JavaScript parser; computed references stay out of scope.
// 这是针对当前语法的限定词法扫描器，并非 JavaScript 解析器；计算引用不在覆盖范围内。
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
for (const scope of manifest.sourceScopes) {
  if (!scope.startsWith('apps/')) throw new Error(`Source scope must be inside apps: ${scope}`);
  sourceFiles.push(...(await listFiles(resolve(repositoryRoot, scope), manifest.excludedDirectoryNames))
    .filter(path => manifest.sourceExtensions.includes(extname(path))));
}
const ownerCounts = Object.fromEntries(Object.keys(manifest.roles).map(role => [role, 0]));
for (const path of sourceFiles) {
  const source = toRepositoryPath(path);
  const owners = ownersOf(source);
  if (owners.length !== 1 || !Object.hasOwn(manifest.roles, owners[0])) violations.push({ code: 'SOURCE_OWNERSHIP', source, owners });
  else ownerCounts[owners[0]]++;
}

const gatewayRoot = resolve(repositoryRoot, 'apps/model-gateway');
const domainOf = path => {
  const local = relative(gatewayRoot, path).split(sep);
  return manifest.gatewayDomains.includes(local[0]) ? local[0] : null;
};
const gatewayFiles = sourceFiles.filter(path => path.startsWith(`${gatewayRoot}${sep}`) && extname(path) === '.mjs' &&
  (domainOf(path) || Object.hasOwn(manifest.stableEntrypoints, toRepositoryPath(path)) ||
    manifest.additionalGatewayModules?.includes(toRepositoryPath(path))));
const domainEdges = new Map(manifest.gatewayDomains.map(domain => [domain, new Set()]));
let relativeModuleCount = 0;
let relativeResourceCount = 0;
const optionalResources = [];
const referencesByFile = new Map();
for (const path of gatewayFiles) {
  const source = toRepositoryPath(path);
  const references = staticReferences(await readFile(path, 'utf8'));
  referencesByFile.set(source, references);
  for (const reference of references) {
    if (reference.kind === 'module') relativeModuleCount++;
    else relativeResourceCount++;
    const target = resolve(dirname(path), reference.specifier);
    let exists = false;
    try {
      const targetStatus = await stat(target);
      exists = reference.kind === 'resource' || targetStatus.isFile();
    } catch (error) { if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error; }
    const optional = reference.kind === 'resource' && manifest.optionalRelativeResources.some(entry =>
      entry.source === source && entry.specifier === reference.specifier);
    if (!exists && optional) optionalResources.push({ source, specifier: reference.specifier });
    else if (!exists) violations.push({ code: 'MISSING_RELATIVE_TARGET', source, ...reference, target: toRepositoryPath(target) });
    const sourceDomain = domainOf(path);
    const targetDomain = domainOf(target);
    if (reference.kind === 'module' && sourceDomain && targetDomain && sourceDomain !== targetDomain) {
      domainEdges.get(sourceDomain).add(targetDomain);
      if (!manifest.allowedDomainDependencies[sourceDomain].includes(targetDomain)) {
        violations.push({ code: 'FORBIDDEN_DOMAIN_DEPENDENCY', source, target: toRepositoryPath(target), sourceDomain, targetDomain });
      }
    }
  }
}

// Domain cycles are checked independently of individual file cycles and allowed edge declarations.
// 域循环独立于单文件循环和允许边声明进行检查。
const activeDomains = [];
const visitedDomains = new Set();
function visitDomain(domain) {
  if (activeDomains.includes(domain)) {
    violations.push({ code: 'DOMAIN_CYCLE', domains: [...activeDomains.slice(activeDomains.indexOf(domain)), domain] });
    return;
  }
  if (visitedDomains.has(domain)) return;
  activeDomains.push(domain);
  for (const dependency of domainEdges.get(domain)) visitDomain(dependency);
  activeDomains.pop();
  visitedDomains.add(domain);
}
for (const domain of manifest.gatewayDomains) visitDomain(domain);
for (const [source, expected] of Object.entries(manifest.stableEntrypoints)) {
  if (!referencesByFile.get(source)?.some(reference => reference.kind === 'module' && reference.specifier === expected)) {
    violations.push({ code: 'STABLE_ENTRYPOINT_TARGET', source, expected });
  }
}

// Tests inherit their direct implementation dependencies; cross-domain tests retain all relevant owners.
// 测试继承直接实现依赖的负责人；跨域测试保留所有相关角色。
const testSuites = new Map();
let testSourceCount = 0;
for (const testRoot of [manifest.testAttribution.gatewayRoot, manifest.testAttribution.smokeRoot]) {
  for (const path of await listFiles(resolve(repositoryRoot, testRoot), ['node_modules', 'bin', 'obj', '.git'])) {
    if (!['.mjs', '.csproj'].includes(extname(path))) continue;
    testSourceCount++;
    const suite = testRoot === manifest.testAttribution.gatewayRoot ? toRepositoryPath(path) :
      `${testRoot}/${relative(resolve(repositoryRoot, testRoot), path).split(sep)[0]}`;
    const owners = testSuites.get(suite) ?? new Set();
    const source = await readFile(path, 'utf8');
    const targets = extname(path) === '.mjs' ? staticReferences(source).filter(reference => reference.kind === 'module').map(reference => reference.specifier) :
      [...source.matchAll(/<Compile\s+Include="([^"]+)"/g)].map(match => match[1].replaceAll('\\', '/'));
    for (const target of targets) {
      for (const owner of ownersOf(toRepositoryPath(resolve(dirname(path), target)))) owners.add(owner);
    }
    testSuites.set(suite, owners);
  }
}
const testAssignments = [...testSuites].sort(([first], [second]) => first.localeCompare(second)).map(([suite, owners]) => ({
  suite, roles: owners.size ? [...owners].sort() : [manifest.testAttribution.fallbackRole],
  attribution: owners.size ? 'direct-production-dependency' : 'integration-coordination'
}));
const report = {
  passed: violations.length === 0,
  productionSourceFiles: sourceFiles.length,
  ownershipByRole: ownerCounts,
  gatewayModuleFiles: gatewayFiles.length,
  relativeModuleReferences: relativeModuleCount,
  relativeResourceReferences: relativeResourceCount,
  domainDependencies: Object.fromEntries([...domainEdges].map(([domain, edges]) => [domain, [...edges].sort()])),
  absentOptionalResources: optionalResources,
  testSourceOrProjectFiles: testSourceCount,
  testSuiteAssignments: testAssignments.length,
  testAssignmentsByRole: Object.fromEntries(Object.keys(manifest.roles).map(role =>
    [role, testAssignments.filter(assignment => assignment.roles.includes(role)).length])),
  detailsCommand: 'node tools/development/check-architecture.mjs --details',
  ...(process.argv.includes('--details') ? { testAssignments } : {}),
  coverageNotes: manifest.coverageNotes,
  violations
};
console.log(JSON.stringify(report, null, 2));
process.exitCode = violations.length ? 1 : 0;
