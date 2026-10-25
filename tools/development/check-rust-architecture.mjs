import { readFile, stat } from 'node:fs/promises';
import { basename, dirname, extname, isAbsolute, relative, resolve, sep } from 'node:path';

const toRelativePath = (repositoryRoot, path) => relative(repositoryRoot, path).split(sep).join('/');
const isInside = (root, path) => path === root || path.startsWith(`${root}${sep}`);

// Skip literals and nested comments so examples do not become dependencies.
// 跳过字面量和嵌套注释，避免把示例当作依赖。
// This scans static structure; Rust resolves names, macros and cfg conditions.
// 此处仅扫描静态结构，名称、宏和 cfg 条件由 Rust 解析。
function rustTokens(source) {
  const tokens = [];
  let offset = 0;
  while (offset < source.length) {
    if (/\s/u.test(source[offset])) { offset++; continue; }
    if (source.startsWith('//', offset)) {
      const end = source.indexOf('\n', offset + 2);
      offset = end < 0 ? source.length : end + 1;
      continue;
    }
    if (source.startsWith('/*', offset)) {
      let depth = 1;
      offset += 2;
      while (offset < source.length && depth) {
        if (source.startsWith('/*', offset)) { depth++; offset += 2; }
        else if (source.startsWith('*/', offset)) { depth--; offset += 2; }
        else offset++;
      }
      continue;
    }
    const raw = source.slice(offset).match(/^(?:b|c)?r(#+)?"/u);
    if (raw) {
      const closing = `"${raw[1] ?? ''}`;
      const start = offset + raw[0].length;
      const end = source.indexOf(closing, start);
      tokens.push({ kind: 'string', value: source.slice(start, end < 0 ? source.length : end) });
      offset = end < 0 ? source.length : end + closing.length;
      continue;
    }
    if (source[offset] === '"') {
      const start = offset++;
      while (offset < source.length) {
        if (source[offset] === '\\') offset += 2;
        else if (source[offset++] === '"') break;
      }
      const literal = source.slice(start, offset);
      let value;
      try { value = JSON.parse(literal); } catch { value = literal.slice(1, -1); }
      tokens.push({ kind: 'string', value });
      continue;
    }
    const character = source.slice(offset).match(/^'(?:\\(?:u\{[A-Fa-f0-9_]+\}|x[A-Fa-f0-9]{2}|.)|[^'\\\r\n])'/u);
    if (character) { offset += character[0].length; continue; }
    const identifier = source.slice(offset).match(/^[A-Za-z_][A-Za-z0-9_]*/u);
    if (identifier) {
      tokens.push({ kind: 'identifier', value: identifier[0] });
      offset += identifier[0].length;
    } else {
      const value = source.startsWith('::', offset) ? '::' : source[offset];
      tokens.push({ kind: 'punctuation', value });
      offset += value.length;
    }
  }
  return tokens;
}

// Resolve explicit paths and grouped use trees without expanding macros.
// 解析显式路径和分组 use 树，不展开宏。
function referencedModules(tokens, nativeModule, crateNames, domains, moduleFiles) {
  const module = nativeModule.replace(/\.rs$/u, '').split('/');
  if (module.at(-1) === 'mod') module.pop();
  if (['lib.rs', 'main.rs'].includes(nativeModule) || nativeModule.startsWith('bin/')) module.length = 0;
  const aliases = new Map();
  const references = [];
  function absolute(parts) {
    if (!parts.length) return null;
    let resolved, index = 0;
    if (parts[0] === 'crate' || crateNames.includes(parts[0])) { resolved = []; index = 1; }
    else if (parts[0] === 'self') { resolved = [...module]; index = 1; }
    else if (parts[0] === 'super') {
      resolved = [...module];
      while (parts[index] === 'super') { resolved.pop(); index++; }
    } else if (aliases.has(parts[0])) { resolved = [...aliases.get(parts[0])]; index = 1; }
    else if (domains.includes(parts[0])) resolved = [];
    else return null;
    return [...resolved, ...parts.slice(index)];
  }
  function tree(index, prefix = []) {
    const parts = [...prefix];
    while (tokens[index]?.kind === 'identifier') {
      parts.push(tokens[index++].value);
      if (tokens[index]?.value !== '::') break;
      index++;
    }
    if (tokens[index]?.value === '{') {
      index++;
      while (tokens[index] && tokens[index].value !== '}') {
        if (tokens[index].value === ',') { index++; continue; }
        const next = tree(index, parts);
        if (next === index) index++; else index = next;
      }
      return tokens[index]?.value === '}' ? index + 1 : index;
    }
    let alias = parts.at(-1);
    if (tokens[index]?.value === 'as') { alias = tokens[index + 1]?.value; index += 2; }
    if (tokens[index]?.value === '*') { index++; alias = null; }
    const path = absolute(parts);
    if (path) {
      references.push(path);
      if (alias && alias !== '_') aliases.set(alias, path);
    }
    return index;
  }
  for (let index = 0; index < tokens.length; index++) {
    if (tokens[index].value === 'use') tree(index + 1);
  }
  for (let index = 0; index < tokens.length; index++) {
    if (tokens[index].kind !== 'identifier' || tokens[index - 1]?.value === '::' || tokens[index + 1]?.value !== '::') continue;
    const parts = [tokens[index].value];
    let cursor = index + 2;
    while (tokens[cursor]?.kind === 'identifier') {
      parts.push(tokens[cursor].value); cursor++;
      if (tokens[cursor]?.value !== '::') break;
      cursor++;
    }
    const path = absolute(parts);
    if (path) references.push(path);
  }
  const result = new Map();
  for (const path of references) {
    const domain = domains.includes(path[0]) ? path[0] : null;
    let target = null;
    for (let count = path.length; count > 0; count--) {
      target = moduleFiles.get(path.slice(0, count).join('/'));
      if (target) break;
    }
    result.set(`${domain}:${target}`, { domain, target });
  }
  return [...result.values()];
}

function hasExecutionImport(tokens) {
  const forbidden = ['fs', 'net', 'process', 'thread'];
  const executors = new Set(['rusqlite', 'reqwest', 'axum', 'ort', 'ort_sys', 'usearch']);
  const addressTypes = new Set(['IpAddr', 'Ipv4Addr', 'Ipv6Addr', 'SocketAddr', 'SocketAddrV4', 'SocketAddrV6']);
  function addressOnly(index) {
    if (tokens[index]?.value !== '::') return false;
    if (addressTypes.has(tokens[index + 1]?.value)) return true;
    if (tokens[index + 1]?.value !== '{') return false;
    let cursor = index + 2, count = 0;
    while (cursor < tokens.length && tokens[cursor].value !== '}') {
      if (tokens[cursor].value === ',') { cursor++; continue; }
      if (!addressTypes.has(tokens[cursor].value)) return false;
      count++; cursor++;
      if (tokens[cursor]?.value === 'as') {
        cursor++;
        if (tokens[cursor]?.kind !== 'identifier') return false;
        cursor++;
      }
    }
    return count > 0 && tokens[cursor]?.value === '}';
  }
  for (let index = 0; index < tokens.length; index++) {
    if (tokens[index].kind === 'identifier' && executors.has(tokens[index].value) && tokens[index + 1]?.value === '::') return true;
    if (tokens[index].value === 'tokio' && tokens[index + 1]?.value === '::' && ['fs', 'net', 'process', 'task'].includes(tokens[index + 2]?.value)) return true;
    if (tokens[index].kind !== 'identifier' || tokens[index].value !== 'std' || tokens[index + 1]?.value !== '::') continue;
    // Address value types parse endpoints without opening a network connection.
    // 地址值类型只解析端点，不打开网络连接；通配导入和实际 I/O 仍禁止。
    if (forbidden.includes(tokens[index + 2]?.value) &&
      !(tokens[index + 2].value === 'net' && addressOnly(index + 3))) return true;
    if (tokens[index + 2]?.value !== '{') continue;
    let depth = 1;
    for (let cursor = index + 3; cursor < tokens.length && depth; cursor++) {
      const value = tokens[cursor].value;
      if (value === '{') depth++;
      else if (value === '}') depth--;
      else if (depth === 1 && forbidden.includes(value) && !(value === 'net' && addressOnly(cursor + 1))) return true;
    }
  }
  return false;
}

function moduleDeclarations(tokens) {
  const declarations = [];
  for (let index = 0; index < tokens.length; index++) {
    if (tokens[index].kind !== 'identifier' || tokens[index].value !== 'mod' || tokens[index + 1]?.kind !== 'identifier' || tokens[index + 2]?.value !== ';') continue;
    let explicitPath, testOnly = false;
    // A path attribute may precede visibility/cfg attributes; inspect only the current declaration prefix.
    // path 属性可以位于可见性或 cfg 属性之前；只检查当前声明的前缀，避免借用上一项的路径。
    for (let cursor = index - 1; cursor >= 0 && ![';', '{', '}'].includes(tokens[cursor].value); cursor--) {
      if (tokens[cursor].kind === 'identifier' && tokens[cursor].value === 'path' && tokens[cursor + 1]?.value === '=' && tokens[cursor + 2]?.kind === 'string') {
        explicitPath = tokens[cursor + 2].value;
      }
      if (tokens.slice(cursor, cursor + 7).map(token => token.value).join('') === '#[cfg(test)]') testOnly = true;
    }
    declarations.push({ module: tokens[index + 1].value, explicitPath, testOnly });
  }
  return declarations;
}

function cargoDependencies(source) {
  const dependencies = new Map();
  let section = '';
  let dependencyTableName = null;
  for (const line of source.split(/\r?\n/u)) {
    const header = line.match(/^\s*\[([^\]]+)\]/u);
    if (header) {
      section = header[1];
      const dependencyTable = section.match(/^(?:target\..+\.)?(?:dependencies|build-dependencies)\.([A-Za-z0-9_-]+)$/u);
      dependencyTableName = dependencyTable?.[1] ?? null;
      if (dependencyTableName) dependencies.set(dependencyTableName, null);
      continue;
    }
    const renamedPackage = line.match(/\bpackage\s*=\s*"([A-Za-z0-9_-]+)"/u)?.[1];
    if (dependencyTableName && renamedPackage) {
      const version = dependencies.get(dependencyTableName);
      dependencies.delete(dependencyTableName);
      dependencies.set(renamedPackage, version);
      dependencyTableName = renamedPackage;
      continue;
    }
    if (dependencyTableName) {
      const version = line.match(/^\s*version\s*=\s*"([^"]+)"/u)?.[1];
      if (version) dependencies.set(dependencyTableName, version);
      continue;
    }
    if (!/^(?:target\..+\.)?(?:dependencies|build-dependencies)$/u.test(section)) continue;
    const entry = line.match(/^\s*([A-Za-z0-9_-]+|"[A-Za-z0-9_-]+")\s*=/u);
    if (entry) {
      const version = line.match(/=\s*"([^"]+)"/u)?.[1] ?? line.match(/\bversion\s*=\s*"([^"]+)"/u)?.[1] ?? null;
      dependencies.set(renamedPackage ?? entry[1].replaceAll('"', ''), version);
    }
  }
  return dependencies;
}

async function isFile(path) {
  try { return (await stat(path)).isFile(); }
  catch (error) { if (['ENOENT', 'ENOTDIR'].includes(error.code)) return false; throw error; }
}

/** Use the shared architecture configuration to check native-core boundaries.
 * 使用同一技术配置检查原生核心边界。 */
export async function checkRustArchitecture({ repositoryRoot, manifest, sourceFiles }) {
  const violations = [];
  const localTestFiles = new Set((manifest.localTestFiles ?? []).map(path => resolve(repositoryRoot, path)));
  let sourceFileCount = 0, moduleReferences = 0, domainReferences = 0, dependencyCount = 0, integrationModuleCount = 0, integrationReferences = 0, executionModuleCount = 0;
  const domainDependencies = {};
  for (const application of manifest.rustApps ?? []) {
    const root = resolve(repositoryRoot, application.root);
    if (isAbsolute(application.root) || !application.root.startsWith('apps/') || !isInside(repositoryRoot, root)) {
      violations.push({ code: 'RUST_SOURCE_SCOPE', source: application.root });
      continue;
    }
    const rustRoot = resolve(root, 'src');
    const domains = application.domains ?? [];
    const edges = new Map(domains.map(domain => [domain, new Set()]));
    const nativeFiles = sourceFiles.filter(path => isInside(rustRoot, path) && extname(path) === '.rs');
    const localName = path => relative(rustRoot, path).split(sep).join('/');
    const moduleFiles = new Map(nativeFiles.map(path => [localName(path).replace(/(?:\/mod)?\.rs$/u, ''), localName(path)]));
    const integration = new Map();
    for (const entry of application.integrationModules ?? []) {
      integrationModuleCount++;
      if (!entry.path || isAbsolute(entry.path) || entry.path.includes('..') || entry.path.includes('\\') || !entry.path.endsWith('.rs') || integration.has(entry.path) ||
          !entry.purpose || !Array.isArray(entry.allowedDomains) || entry.allowedDomains.some(domain => !domains.includes(domain))) {
        violations.push({ code: 'RUST_INVALID_INTEGRATION_DECLARATION', source: application.root, module: entry.path });
        continue;
      }
      integration.set(entry.path, entry);
      if (!nativeFiles.some(path => localName(path) === entry.path)) violations.push({ code: 'RUST_INTEGRATION_MODULE_NOT_INVENTORIED', source: `${application.root}/src/${entry.path}` });
    }
    const execution = new Set();
    for (const name of application.executionModules ?? []) {
      executionModuleCount++;
      if (isAbsolute(name) || name.includes('..') || name.includes('\\') || execution.has(name) || !nativeFiles.some(path => localName(path) === name))
        violations.push({ code: 'RUST_INVALID_EXECUTION_DECLARATION', source: `${application.root}/src/${name}` });
      execution.add(name);
    }
    const cargoPath = resolve(root, 'Cargo.toml');
    if (!await isFile(cargoPath)) {
      violations.push({ code: 'RUST_MANIFEST_MISSING', source: toRelativePath(repositoryRoot, cargoPath) });
      continue;
    }
    const cargo = await readFile(cargoPath, 'utf8');
    const packageName = cargo.match(/^name\s*=\s*"([A-Za-z0-9_-]+)"/mu)?.[1]?.replaceAll('-', '_');
    const libraryName = cargo.match(/\[lib\][\s\S]*?^name\s*=\s*"([A-Za-z0-9_-]+)"/mu)?.[1]?.replaceAll('-', '_');
    for (const [dependency, version] of cargoDependencies(cargo)) {
      dependencyCount++;
      if (!application.allowedDependencies?.includes(dependency)) violations.push({
        code: 'RUST_UNDECLARED_CAPABILITY_DEPENDENCY', source: toRelativePath(repositoryRoot, cargoPath), dependency
      });
      if (!version?.startsWith('=') || version !== application.dependencyVersions?.[dependency]) violations.push({
        code: 'RUST_DEPENDENCY_VERSION', source: toRelativePath(repositoryRoot, cargoPath), dependency, version, expected: application.dependencyVersions?.[dependency]
      });
    }
    for (const path of nativeFiles) {
      sourceFileCount++;
      const source = toRelativePath(repositoryRoot, path);
      const local = relative(rustRoot, path).split(sep);
      const domain = domains.includes(local[0]) ? local[0] : null;
      const sourceText = await readFile(path, 'utf8');
      const tokens = rustTokens(sourceText);
      const testOnly = basename(path).endsWith('_tests.rs') && /^\s*#!\[cfg\(test\)\]/u.test(sourceText);
      const nativeModule = relative(rustRoot, path).split(sep).join('/');
      const adapter = integration.get(nativeModule);
      // Integration effects do not create policy-domain edges; the reverse direction remains forbidden.
      // 集成执行调用不构成纯规则域依赖；纯规则反向依赖集成层仍然禁止，不能用例外掩盖循环。
      if (!testOnly && hasExecutionImport(tokens) && !execution.has(nativeModule))
        violations.push({ code: 'RUST_DOMAIN_EXECUTION_DEPENDENCY', source, domain });
      for (const reference of referencedModules(tokens, nativeModule, [packageName, libraryName].filter(Boolean), domains, moduleFiles)) {
        const targetDomain = reference.domain;
        if (!adapter && !testOnly && reference.target && integration.has(reference.target)) violations.push({
          code: 'RUST_POLICY_INTEGRATION_DEPENDENCY', source, target: `${application.root}/src/${reference.target}`
        });
        if (!targetDomain || domain === targetDomain) continue;
        domainReferences++;
        if (adapter) {
          integrationReferences++;
          if (!adapter.allowedDomains.includes(targetDomain)) violations.push({ code: 'RUST_FORBIDDEN_INTEGRATION_DOMAIN', source, targetDomain });
          continue;
        }
        if (!domain) continue;
        edges.get(domain).add(targetDomain);
        if (!application.allowedDomainDependencies?.[domain]?.includes(targetDomain)) violations.push({
          code: 'RUST_FORBIDDEN_DOMAIN_DEPENDENCY', source, sourceDomain: domain, targetDomain
        });
      }
      for (const declaration of moduleDeclarations(tokens)) {
        moduleReferences++;
        const base = ['lib.rs', 'main.rs', 'mod.rs'].includes(basename(path)) ? dirname(path) : resolve(dirname(path), basename(path, '.rs'));
        const candidates = declaration.explicitPath
          ? [resolve(dirname(path), declaration.explicitPath)]
          : [resolve(base, `${declaration.module}.rs`), resolve(base, declaration.module, 'mod.rs')];
        let target;
        for (const candidate of candidates) {
          if (await isFile(candidate)) { target = candidate; break; }
        }
        // Only declared cfg(test) modules may be absent from a production-only checkout.
        // 只有清单内且受 cfg(test) 保护的模块才允许不随生产源码提供。
        if (declaration.testOnly && candidates.some(candidate => isInside(rustRoot, candidate) && localTestFiles.has(candidate)) &&
            (!target || localTestFiles.has(target))) continue;
        if (!target || extname(target) !== '.rs') violations.push({ code: 'RUST_MODULE_TARGET_MISSING', source, module: declaration.module });
        else if (!isInside(rustRoot, target)) violations.push({ code: 'RUST_MODULE_SCOPE_ESCAPE', source, target: toRelativePath(repositoryRoot, target) });
        else if (!sourceFiles.includes(target)) violations.push({ code: 'RUST_MODULE_TARGET_NOT_INVENTORIED', source, target: toRelativePath(repositoryRoot, target) });
      }
    }
    const visited = new Set(), active = [];
    function visit(domain) {
      if (active.includes(domain)) {
        violations.push({ code: 'RUST_DOMAIN_CYCLE', source: application.root, domains: [...active.slice(active.indexOf(domain)), domain] });
        return;
      }
      if (visited.has(domain)) return;
      active.push(domain);
      for (const dependency of edges.get(domain)) visit(dependency);
      active.pop(); visited.add(domain);
    }
    for (const domain of domains) visit(domain);
    domainDependencies[application.root] = Object.fromEntries([...edges].map(([domain, dependencies]) => [domain, [...dependencies].sort()]));
  }
  return { violations, sourceFileCount, moduleReferences, domainReferences, dependencyCount, integrationModuleCount, integrationReferences, executionModuleCount, domainDependencies };
}
