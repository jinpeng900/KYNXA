import { createHash } from 'node:crypto';
import { mkdtemp, opendir, realpath, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ensureLocalDirectory, inspectLocalPath, toolFailure, within } from './tool-paths.mjs';
import { parseSkillFrontmatter, validateSkillFrontmatter } from './skill-frontmatter.mjs';
import { assertSkillResourceAllowed, inspectSkillPackage, readSkillFile, readSkillResource, resolveSkillResource, skillScriptRuntime } from './skill-resources.mjs';
import { checkSkillEnvironment, skillPackagePublicInventory } from './skill-package.mjs';
import { isExtensionManagedPath } from './extension-storage.mjs';

const MAX_SKILL_BYTES = 256 * 1024;
const MAX_SKILLS = 128;
const MAX_CANDIDATES_PER_DIRECTORY = 512;
const decoder = new TextDecoder('utf-8', { fatal: true });
const defaultBundledDirectory = join(dirname(fileURLToPath(import.meta.url)), 'skills');


export class AppSkillService {
  constructor(root, { bundledDirectory = defaultBundledDirectory, ownedDataRoot = root, denyResource } = {}) {
    this.root = resolve(root);
    this.ownedDataRoot = resolve(ownedDataRoot);
    this.bundledDirectory = bundledDirectory === null ? null : resolve(bundledDirectory);
    this.discovery = new WeakMap();
    this.imports = new Map();
    this.resourceDeny = denyResource;
    this.canonicalRoot = this.root;
    this.canonicalDataRoot = this.ownedDataRoot;
    this.canonicalBundledDirectory = this.bundledDirectory;
    this.denyResource = path => this._deniedResource(path);
  }

  async _ensureResourceScope() {
    try { this.canonicalRoot = await realpath(this.root); }
    catch (error) { if (error.code !== 'ENOENT') throw error; this.canonicalRoot = this.root; }
    try { this.canonicalDataRoot = await realpath(this.ownedDataRoot); }
    catch (error) { if (error.code !== 'ENOENT') throw error; this.canonicalDataRoot = this.ownedDataRoot; }
    if (this.bundledDirectory) {
      try { this.canonicalBundledDirectory = await realpath(this.bundledDirectory); }
      catch (error) { if (error.code !== 'ENOENT') throw error; this.canonicalBundledDirectory = this.bundledDirectory; }
    }
  }

  _deniedResource(path) {
    const aliases = new Set([resolve(path)]);
    for (const [lexical, canonical] of [[this.root, this.canonicalRoot], [this.ownedDataRoot, this.canonicalDataRoot]]) {
      if (within(canonical, path)) aliases.add(resolve(lexical, relative(canonical, path)));
      if (within(lexical, path)) aliases.add(resolve(canonical, relative(lexical, path)));
    }
    if ([...aliases].some(alias => this.resourceDeny?.(alias))) return true;
    if ([this.bundledDirectory, this.canonicalBundledDirectory].some(root => root && within(root, path))) return false;
    const extensionRoots = [...new Set([this.root, this.canonicalRoot])], dataRoots = [...new Set([this.ownedDataRoot, this.canonicalDataRoot])];
    // Recovery/control files stay private even if Extensions is itself a managed workspace.
    if ([...aliases].some(alias => extensionRoots.some(root => isExtensionManagedPath(alias, root) &&
        !within(join(root, 'Skills'), alias)))) return true;
    // A configured public Skills directory remains readable when Extensions lives below Data.
    if (extensionRoots.some(root => within(join(root, 'Skills'), path)) &&
        (!dataRoots.some(root => within(root, path)) ||
          extensionRoots.some(extension => dataRoots.some(data => within(data, extension))))) return false;
    for (const root of dataRoots) {
      if (within(join(root, 'Skills'), path)) return false;
      const work = join(root, 'Desktop', 'Projects');
      if (within(work, path)) {
        const id = relative(work, path).split(sep)[0];
        if (/^(?:[a-f0-9]{32}|[a-f0-9]{8}-(?:[a-f0-9]{4}-){3}[a-f0-9]{12})$/i.test(id)) return false;
      }
    }
    return [...extensionRoots, ...dataRoots].some(root => within(root, path));
  }

  _sources(context, config) {
    const seen = new Set();
    return [...(this.bundledDirectory ? [{ path: this.bundledDirectory, origin: 'builtin' }] : []),
      { path: join(this.root, 'Skills'), origin: 'data' },
      ...(config?.skillDirectories ?? []).map(path => ({ path, origin: 'configured' })),
      ...(context?.workspaceRoot ? [{ path: join(context.workspaceRoot, '.kynxa', 'skills'), origin: 'workspace' }] : [])]
      .map((source, priority) => ({ ...source, path: resolve(source.path), priority })).filter(source => {
        const key = this._sourceKey(source.path);
        if (seen.has(key)) return false;
        seen.add(key); return true;
      });
  }

  _sourceKey(path) { return process.platform === 'win32' ? resolve(path).toLowerCase() : resolve(path); }
  async _sourceIdentity(path) {
    try { return this._sourceKey(await realpath(path)); }
    // Unavailable sources still retain their lexical identity and diagnostic; dedup must not block other skills.
    catch { return this._sourceKey(path); }
  }
  roots(context, config) { return this._sources(context, config).map(source => source.path); }

  async _load(file, { includeContent = false, signal } = {}) {
    signal?.throwIfAborted();
    await this._ensureResourceScope();
    assertSkillResourceAllowed(file, this.denyResource);
    const info = await inspectLocalPath(file, { allowMissing: true });
    if (!info) return null;
    if (!info.isFile() || info.size > MAX_SKILL_BYTES) throw toolFailure('应用技能文件过大或结构无效。', 'INVALID_APP_SKILL');
    const data = await readSkillFile(file, { maxBytes: MAX_SKILL_BYTES, signal, denyResource: this.denyResource }), bytes = data.bytes;
    if (bytes.length > MAX_SKILL_BYTES) throw toolFailure('应用技能文件过大。', 'INVALID_APP_SKILL');
    let content;
    try { content = decoder.decode(bytes); } catch { throw toolFailure('应用技能须使用 UTF-8 文本。', 'INVALID_APP_SKILL'); }
    if (content.includes('\0')) throw toolFailure('应用技能不能包含二进制文本。', 'INVALID_APP_SKILL');
    const header = parseSkillFrontmatter(content);
    if (!header) throw toolFailure('应用技能元数据无效，原文件已保留。', 'INVALID_APP_SKILL');
    const validation = validateSkillFrontmatter(content, { directoryName: basename(dirname(file)) });
    const id = createHash('sha256').update(resolve(file)).digest('hex').slice(0, 24);
    return { id, ...header, source: resolve(file), sha256: data.sha256,
      standardCompliant: validation.valid, diagnostics: validation.diagnostics, ...(includeContent ? { content } : {}) };
  }

  async list(context, config, { includeDisabled = false, signal } = {}) {
    await this._ensureResourceScope();
    const skills = [], sources = new Set(), disabled = new Set(config?.disabledSkills ?? []);
    const status = { maxSkills: MAX_SKILLS, maxCandidatesPerDirectory: MAX_CANDIDATES_PER_DIRECTORY,
      truncated: false, unavailableCount: 0, disabledCount: 0, conflictCount: 0 };
    const add = async (skill, source) => {
      if (!skill) return;
      const identity = await this._sourceIdentity(skill.source);
      if (!sources.has(identity)) {
        sources.add(identity);
        skills.push({ ...skill, origin: source.origin, priority: source.priority, enabled: !disabled.has(skill.id) });
      }
    };
    for (const source of this._sources(context, config)) {
      signal?.throwIfAborted();
      const root = source.path;
      if (skills.length >= MAX_SKILLS) { status.truncated = true; break; }
      let candidates;
      try {
        assertSkillResourceAllowed(root, this.denyResource);
        const info = await inspectLocalPath(root, { allowMissing: true });
        if (!info) continue;
        if (!info.isDirectory()) throw toolFailure('应用技能路径不是目录。', 'INVALID_APP_SKILL');
        candidates = [join(root, 'SKILL.md')];
        let visited = 0;
        for await (const entry of await opendir(root)) {
          signal?.throwIfAborted();
          if (visited++ >= MAX_CANDIDATES_PER_DIRECTORY) { status.truncated = true; break; }
          if (!entry.isSymbolicLink() && entry.isDirectory()) candidates.push(join(root, entry.name, 'SKILL.md'));
        }
        candidates.sort((left, right) => left.localeCompare(right));
      } catch (error) {
        if (signal?.aborted || error.name === 'AbortError') throw error;
        status.unavailableCount++;
        await add({ id: createHash('sha256').update(join(root, 'SKILL.md')).digest('hex').slice(0, 24), name: basename(root).slice(0, 100) || 'Unavailable skill directory',
          description: `Application skill directory unavailable (${error.code ?? 'APP_SKILL_UNAVAILABLE'}); original files preserved.`, source: join(root, 'SKILL.md'),
          status: 'unavailable', standardCompliant: false, diagnostics: [{ code: error.code ?? 'APP_SKILL_UNAVAILABLE', message: 'Skill directory unavailable; originals preserved.' }] }, source);
        continue;
      }
      for (const file of candidates) {
        if (skills.length >= MAX_SKILLS) { status.truncated = true; break; }
        try { await add(await this._load(file, { signal }), source); }
        catch (error) {
          if (signal?.aborted || error.name === 'AbortError') throw error;
          status.unavailableCount++;
          await add({ id: createHash('sha256').update(resolve(file)).digest('hex').slice(0, 24), name: basename(dirname(file)).slice(0, 100) || 'Unavailable application skill',
            description: `Application skill unavailable (${error.code ?? 'APP_SKILL_UNAVAILABLE'}); original file preserved.`, source: resolve(file),
            status: 'unavailable', standardCompliant: false, diagnostics: [{ code: error.code ?? 'APP_SKILL_UNAVAILABLE', message: 'Skill unavailable; original file preserved.' }] }, source);
        }
      }
    }
    const names = new Map();
    for (const skill of skills.filter(item => item.status !== 'unavailable')) {
      const name = skill.name.normalize('NFKC').toLowerCase();
      if (!names.has(name)) names.set(name, []);
      names.get(name).push(skill);
    }
    for (const conflicts of names.values()) if (conflicts.length > 1) {
      status.conflictCount++;
      const preferred = conflicts.find(skill => skill.enabled) ?? conflicts[0];
      for (const skill of conflicts) {
        skill.conflict = { preferredId: preferred.id, preferred: skill.id === preferred.id, ids: conflicts.map(item => item.id) };
        skill.diagnostics.push({ code: 'SKILL_NAME_CONFLICT', severity: 'warning',
          message: 'Multiple sources use this name; IDs remain distinct and discovery priority is explicit.' });
      }
    }
    status.disabledCount = skills.filter(skill => !skill.enabled).length;
    const available = includeDisabled ? skills : skills.filter(skill => skill.enabled);
    this.discovery.set(available, status);
    return available;
  }

  async read(id, context, config, { signal } = {}) {
    if (typeof id !== 'string' || !/^[a-f0-9]{24}$/.test(id)) throw toolFailure('应用技能 ID 无效。', 'APP_SKILL_NOT_FOUND', 404);
    const available = await this.list(context, config, { includeDisabled: true, signal }), source = available.find(skill => skill.id === id);
    if (!source) throw toolFailure('应用技能不存在或不属于当前范围。', 'APP_SKILL_NOT_FOUND', 404);
    if (!source.enabled) throw toolFailure('应用技能已禁用。', 'APP_SKILL_DISABLED', 403);
    if (source.status === 'unavailable') throw toolFailure(source.description, 'INVALID_APP_SKILL');
    const skill = await this._load(source.source, { includeContent: true, signal });
    if (!skill || skill.id !== id || skill.sha256 !== source.sha256) throw toolFailure('应用技能已变化，请重新读取。', 'APP_SKILL_CHANGED', 409);
    return { ...source, ...skill, diagnostics: source.diagnostics };
  }

  async readResource(id, path, context, config, options = {}) {
    const skill = await this.read(id, context, config, options);
    return { skillId: id, ...await readSkillResource(dirname(skill.source), path, { ...options, denyResource: this.denyResource }) };
  }

  async inspect(id, context, config, { signal } = {}) {
    const skill = await this.read(id, context, config, { signal });
    const inventory = await inspectSkillPackage(dirname(skill.source), { signal, denyResource: this.denyResource });
    const { content, ...metadata } = skill;
    return { ...metadata, ...skillPackagePublicInventory(inventory),
      diagnostics: [...skill.diagnostics, ...inventory.diagnostics], packageValid: inventory.valid };
  }

  async checkEnvironment(id, context, config, { signal, sandboxCapabilities = context?.sandboxCapabilities } = {}) {
    const skill = await this.read(id, context, config, { signal });
    const inventory = await inspectSkillPackage(dirname(skill.source), { signal, denyResource: this.denyResource });
    return checkSkillEnvironment(skill, inventory, { signal, sandboxCapabilities, denyResource: this.denyResource });
  }

  async prepareScript(id, path, context, config, { signal, sandboxCapabilities = context?.sandboxCapabilities } = {}) {
    const skill = await this.read(id, context, config, { signal });
    const target = resolveSkillResource(dirname(skill.source), path), runtime = skillScriptRuntime(target.relativePath);
    if (runtime !== 'node') throw toolFailure('Only Node.js skill scripts are supported by the verified sandbox.', 'APP_SKILL_SCRIPT_UNSUPPORTED');
    const inventory = await inspectSkillPackage(dirname(skill.source), { signal, hashes: true, denyResource: this.denyResource });
    if (inventory.files.some(file => file.relativePath === 'SKILL.md' && file.sha256 !== skill.sha256))
      throw toolFailure('Skill metadata changed during snapshot preparation.', 'APP_SKILL_CHANGED', 409);
    if (!inventory.valid) throw Object.assign(toolFailure('Skill package failed snapshot safety checks.', 'UNSAFE_SKILL_PACKAGE', 403), { diagnostics: inventory.diagnostics });
    const script = inventory.files.find(file => file.relativePath === target.relativePath);
    if (!script) throw toolFailure('Skill script was not found in the validated package.', 'SKILL_RESOURCE_NOT_FOUND', 404);
    const environment = await checkSkillEnvironment(skill, inventory, { signal, sandboxCapabilities, scriptPath: target.relativePath, denyResource: this.denyResource });
    return { skillId: id, skillRoot: inventory.skillRoot, scriptPath: target.path, scriptRelativePath: target.relativePath,
      runtime, files: inventory.files, totalBytes: inventory.totalBytes, diagnostics: environment.diagnostics, environment };
  }

  async validateImport(directory, { signal, strict = true } = {}) {
    if (typeof directory !== 'string' || !isAbsolute(directory) || /[\0\r\n]/.test(directory))
      throw toolFailure('Import requires an absolute skill package directory.', 'INVALID_APP_SKILL_IMPORT');
    directory = resolve(directory);
    const skill = await this._load(join(directory, 'SKILL.md'), { includeContent: true, signal });
    if (!skill) throw toolFailure('Imported directory must contain SKILL.md.', 'INVALID_APP_SKILL_IMPORT');
    const inventory = await inspectSkillPackage(directory, { signal, hashes: true, denyResource: this.denyResource });
    if (inventory.files.some(file => file.relativePath === 'SKILL.md' && file.sha256 !== skill.sha256))
      throw toolFailure('Skill metadata changed during import validation.', 'APP_SKILL_CHANGED', 409);
    const diagnostics = [...skill.diagnostics, ...inventory.diagnostics], valid = skill.standardCompliant && inventory.valid;
    if (strict && !valid) throw Object.assign(toolFailure('Skill import requires standard metadata and a safe package.', 'INVALID_APP_SKILL_IMPORT'), { diagnostics });
    const { content, ...metadata } = skill;
    return { valid, skill: metadata, skillRoot: directory, files: inventory.files, totalBytes: inventory.totalBytes, diagnostics };
  }

  async importPackage(directory, options = {}) {
    const validated = await this.validateImport(directory, { ...options, strict: true });
    const name = validated.skill.name.normalize('NFKC'), target = join(this.root, 'Skills', name);
    const previous = this.imports.get(this._sourceKey(target)) ?? Promise.resolve();
    const pending = previous.catch(() => {}).then(() => this._importValidated(validated, target, options));
    this.imports.set(this._sourceKey(target), pending);
    try { return await pending; }
    finally { if (this.imports.get(this._sourceKey(target)) === pending) this.imports.delete(this._sourceKey(target)); }
  }

  async _importValidated(validated, target, { signal } = {}) {
    signal?.throwIfAborted();
    const fingerprint = files => JSON.stringify(files.map(({ relativePath, size, sha256 }) => ({ relativePath, size, sha256 })));
    const result = async (reused, directory = target, origin = 'data') => ({ imported: !reused, reused,
      // Once rename commits, cancellation cannot turn a completed installation into a failed report.
      skill: { ...await this._load(join(directory, 'SKILL.md'), { signal: reused ? signal : undefined }), origin, enabled: true } });
    if (this.bundledDirectory) {
      const bundled = join(this.bundledDirectory, basename(target));
      const existing = await inspectLocalPath(bundled, { allowMissing: true });
      if (existing?.isDirectory()) {
        const inventory = await inspectSkillPackage(bundled, { signal, hashes: true, denyResource: this.denyResource });
        if (inventory.valid && fingerprint(inventory.files) === fingerprint(validated.files))
          return result(true, bundled, 'builtin');
      }
    }
    const equal = async () => {
      const existing = await inspectLocalPath(target, { allowMissing: true });
      if (!existing) return false;
      if (!existing.isDirectory()) throw toolFailure('A different skill package already uses this name.', 'APP_SKILL_CONFLICT', 409);
      const inventory = await inspectSkillPackage(target, { signal, hashes: true, denyResource: this.denyResource });
      if (!inventory.valid || fingerprint(inventory.files) !== fingerprint(validated.files))
        throw toolFailure('A different skill package already uses this name; existing files were preserved.', 'APP_SKILL_CONFLICT', 409);
      return true;
    };
    if (await equal()) return result(true);
    await ensureLocalDirectory(join(this.root, 'Skills'));
    const stagingParent = join(this.root, 'Agent', 'skill-imports');
    await ensureLocalDirectory(stagingParent);
    const stage = await mkdtemp(join(stagingParent, 'package-'));
    try {
      for (const file of validated.files) {
        signal?.throwIfAborted();
        const data = await readSkillFile(file.path, { signal, denyResource: this.denyResource });
        if (data.sha256 !== file.sha256 || data.size !== file.size)
          throw toolFailure('Skill package changed after validation; import was cancelled.', 'APP_SKILL_CHANGED', 409);
        const destination = resolveSkillResource(stage, file.relativePath).path;
        await ensureLocalDirectory(dirname(destination));
        await writeFile(destination, data.bytes, { flag: 'wx', mode: 0o600, signal });
      }
      signal?.throwIfAborted();
      if (await equal()) return result(true);
      // The gateway is the only writer of Data/Skills; rename commits the complete staged package.
      try { await rename(stage, target); }
      catch (error) {
        if (['EEXIST', 'ENOTEMPTY', 'EPERM'].includes(error.code) && await equal()) return result(true);
        throw error;
      }
      return result(false);
    } finally {
      if (!within(stagingParent, stage) || resolve(stage) === resolve(stagingParent))
        throw toolFailure('Invalid skill staging cleanup path.', 'UNSAFE_SKILL_RESOURCE_PATH', 403);
      await rm(stage, { recursive: true, force: true });
    }
  }
}
