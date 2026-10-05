import { lstat, mkdir, readFile } from 'node:fs/promises';
import { join, relative, resolve, sep } from 'node:path';
import { atomicJson } from '../../platform/atomic-json.mjs';
import { validateId } from '../../platform/conversation-id.mjs';
import { RETRIEVAL_SCHEMA_VERSION, requireKeys, retrievalFailure, retrievalRecord } from './retrieval-contracts.mjs';

const queues = new Map();
const MAX_SETTINGS_BYTES = 128 * 1024;

export const DEFAULT_RETRIEVAL_SETTINGS = Object.freeze({
  schemaVersion: RETRIEVAL_SCHEMA_VERSION, revision: 0,
  local: Object.freeze({ enabled: true, semantic: 'auto', vectorBackend: 'sqlite',
    embeddingProfileId: 'builtin-multilingual', rerankProfileId: null }),
  web: Object.freeze({ mode: 'auto', providerId: 'auto', depth: 'standard', language: 'auto', browserRead: 'auto' }),
  cache: Object.freeze({ memoryLimitBytes: 64 * 1024 * 1024, diskLimitBytes: 512 * 1024 * 1024 })
});

function boolean(value, name) {
  if (typeof value !== 'boolean') throw retrievalFailure(`${name} must be boolean. / 开关配置必须为布尔值。`);
  return value;
}

function choice(value, allowed, name) {
  if (!allowed.includes(value)) throw retrievalFailure(`Invalid ${name}. / 配置选项无效。`);
  return value;
}

function reference(value, name, nullable = false) {
  if (nullable && value === null) return null;
  if (typeof value !== 'string' || !/^[a-zA-Z0-9][a-zA-Z0-9._:/-]{0,255}$/.test(value))
    throw retrievalFailure(`Invalid ${name} reference. / 能力引用格式无效。`);
  return value;
}

function integer(value, minimum, maximum, name) {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum)
    throw retrievalFailure(`Invalid ${name} range. / 数值配置超出允许范围。`);
  return value;
}

function settingsPatch(value) {
  requireKeys(value, ['local', 'web', 'cache'], 'retrieval settings');
  const result = {};
  if (value.local !== undefined) {
    requireKeys(value.local, ['enabled', 'semantic', 'vectorBackend', 'embeddingProfileId', 'rerankProfileId'], 'local');
    const local = {};
    for (const [name, item] of Object.entries(value.local)) {
      if (name === 'enabled') local[name] = boolean(item, name);
      else if (name === 'semantic') local[name] = choice(item, ['auto', 'off'], name);
      else if (name === 'vectorBackend') local[name] = choice(item, ['sqlite'], name);
      else local[name] = reference(item, name, true);
    }
    result.local = local;
  }
  if (value.web !== undefined) {
    requireKeys(value.web, ['mode', 'providerId', 'depth', 'language', 'browserRead'], 'web');
    const web = {};
    for (const [name, item] of Object.entries(value.web)) {
      if (name === 'mode' || name === 'browserRead') web[name] = choice(item, ['auto', 'off'], name);
      else if (name === 'depth') web[name] = choice(item, ['standard', 'deep'], name);
      else if (name === 'language') web[name] = choice(item, ['auto', 'zh-CN', 'en', 'en-US'], name);
      else web[name] = reference(item, name);
    }
    result.web = web;
  }
  if (value.cache !== undefined) {
    requireKeys(value.cache, ['memoryLimitBytes', 'diskLimitBytes'], 'cache');
    const cache = {};
    for (const [name, item] of Object.entries(value.cache))
      cache[name] = integer(item, 0, name === 'memoryLimitBytes' ? 512 * 1024 * 1024 : 8 * 1024 * 1024 * 1024, name);
    result.cache = cache;
  }
  return result;
}

function mergeSettings(base, patch) {
  return { ...base, local: { ...base.local, ...patch.local }, web: { ...base.web, ...patch.web },
    cache: { ...base.cache, ...patch.cache } };
}

function projectPatch(value) {
  requireKeys(value, ['overrides', 'indexingSources'], 'project retrieval settings');
  const result = {};
  if (value.overrides !== undefined) {
    requireKeys(value.overrides, ['local', 'web', 'cache'], 'project overrides');
    const updates = Object.fromEntries(Object.entries(value.overrides).filter(([, item]) => item !== null));
    result.overrides = settingsPatch(updates);
    for (const [name, item] of Object.entries(value.overrides)) if (item === null) result.overrides[name] = null;
  }
  if (value.indexingSources !== undefined) {
    requireKeys(value.indexingSources, ['mountedFolder', 'knowledgeIds'], 'indexing sources');
    const indexingSources = {};
    if (value.indexingSources.mountedFolder !== undefined) {
      // Binding revisions are assigned by the owner, never by a UI override.
      // 绑定版本由后端分配，界面覆盖配置不能伪造绑定版本。
      requireKeys(value.indexingSources.mountedFolder, ['enabled'], 'mounted folder');
      indexingSources.mountedFolder = { enabled: boolean(value.indexingSources.mountedFolder.enabled, 'mountedFolder.enabled') };
    }
    if (value.indexingSources.knowledgeIds !== undefined) {
      const ids = value.indexingSources.knowledgeIds;
      if (!Array.isArray(ids) || ids.length > 1000) throw retrievalFailure('Invalid knowledge selection. / 资料选择列表无效。');
      indexingSources.knowledgeIds = [...new Set(ids.map(id => reference(id, 'knowledgeId')))];
    }
    result.indexingSources = indexingSources;
  }
  return result;
}

function defaultProject(projectId) {
  return { schemaVersion: RETRIEVAL_SCHEMA_VERSION, projectId, revision: 0, overrides: {},
    indexingSources: { mountedFolder: { enabled: false, bindingRevision: 0 }, knowledgeIds: [] } };
}

function validateDocument(value, projectId = null) {
  retrievalRecord(value, 'Stored retrieval settings');
  if (value.schemaVersion !== RETRIEVAL_SCHEMA_VERSION)
    throw retrievalFailure('Unsupported retrieval settings version. / 检索配置版本不受支持，原文件已保留。', 'UNSUPPORTED_RETRIEVAL_SETTINGS_VERSION', 409);
  integer(value.revision, 0, Number.MAX_SAFE_INTEGER, 'revision');
  if (projectId === null) {
    requireKeys(value, ['schemaVersion', 'revision', 'local', 'web', 'cache'], 'stored global settings');
    return { ...mergeSettings(structuredClone(DEFAULT_RETRIEVAL_SETTINGS), settingsPatch({ local: value.local,
      web: value.web, cache: value.cache })), revision: value.revision };
  }
  requireKeys(value, ['schemaVersion', 'projectId', 'revision', 'overrides', 'indexingSources'], 'stored project settings');
  if (value.projectId !== projectId) throw retrievalFailure('Project settings identity mismatch. / 工作配置身份不匹配。', 'CORRUPT_RETRIEVAL_SETTINGS', 409);
  const defaults = defaultProject(projectId), overrides = settingsPatch(value.overrides ?? {});
  const sources = value.indexingSources ?? defaults.indexingSources;
  requireKeys(sources, ['mountedFolder', 'knowledgeIds'], 'stored indexing sources');
  requireKeys(sources.mountedFolder, ['enabled', 'bindingRevision'], 'stored folder settings');
  const clean = projectPatch({ indexingSources: { mountedFolder: { enabled: sources.mountedFolder.enabled },
    knowledgeIds: sources.knowledgeIds ?? [] } }).indexingSources;
  clean.mountedFolder.bindingRevision = integer(sources.mountedFolder.bindingRevision, 0, Number.MAX_SAFE_INTEGER, 'bindingRevision');
  return { ...defaults, revision: value.revision, overrides, indexingSources: clean };
}

/** CAS settings under the canonical catalog guard; no credentials or executable paths.
 * 在正式目录锁内执行 CAS 配置写入，不保存密钥、可执行程序或私有路径。 */
export class RetrievalSettingsStore {
  constructor({ root, conversationStore }) {
    if (!conversationStore?.root || (root && resolve(root) !== resolve(conversationStore.root)))
      throw retrievalFailure('The conversation data owner is required. / 检索配置必须使用正式会话数据所有者。');
    this.root = resolve(conversationStore.root);
    this.conversations = conversationStore;
  }

  async _safe(filename, create = false) {
    const suffix = relative(this.root, filename);
    if (suffix === '..' || suffix.startsWith(`..${sep}`) || resolve(this.root, suffix) !== filename)
      throw retrievalFailure('Unsafe settings path. / 检索配置路径无效。', 'UNSAFE_RETRIEVAL_PATH', 409);
    const parts = suffix.split(sep);
    let current = this.root;
    for (let index = -1; index < parts.length; index++) {
      if (index >= 0) current = join(current, parts[index]);
      const isFile = index === parts.length - 1;
      try {
        const info = await lstat(current);
        if (info.isSymbolicLink() || (isFile ? !info.isFile() || info.nlink > 1 : !info.isDirectory()))
          throw retrievalFailure('Unsafe settings path. / 检索配置包含链接或异常文件结构。', 'UNSAFE_RETRIEVAL_PATH', 409);
        if (isFile && info.size > MAX_SETTINGS_BYTES)
          throw retrievalFailure('Settings file is too large. / 检索配置文件过大，原文件已保留。', 'CORRUPT_RETRIEVAL_SETTINGS', 409);
      } catch (error) {
        if (error.code !== 'ENOENT') throw error;
        if (create && !isFile) await mkdir(current, { mode: 0o700 });
        else return false;
      }
    }
    return true;
  }

  async _read(filename, projectId = null) {
    if (!await this._safe(filename)) return projectId === null ? structuredClone(DEFAULT_RETRIEVAL_SETTINGS) : defaultProject(projectId);
    let value;
    try { value = JSON.parse((await readFile(filename, 'utf8')).replace(/^\uFEFF/, '')); }
    catch (error) {
      if (error instanceof SyntaxError) throw retrievalFailure('Corrupt retrieval settings. / 检索配置格式损坏，原文件已保留。', 'CORRUPT_RETRIEVAL_SETTINGS', 409);
      throw error;
    }
    return validateDocument(value, projectId);
  }

  async _run(filename, operation) {
    const pending = (queues.get(filename) ?? Promise.resolve()).catch(() => {}).then(operation);
    queues.set(filename, pending);
    try { return await pending; }
    finally { if (queues.get(filename) === pending) queues.delete(filename); }
  }

  _global(operation) {
    return this.conversations.withCatalogStorage(() => {
      const filename = join(this.root, 'Retrieval', 'settings.json');
      return this._run(filename, () => operation(filename));
    });
  }

  _project(projectId, operation) {
    projectId = validateId(projectId);
    return this.conversations.withProjectStorage(projectId, relationship => {
      const filename = join(this.root, 'Projects', relationship.projectId, 'retrieval.json');
      return this._run(filename, () => operation(filename, relationship));
    });
  }

  getGlobal() { return this._global(filename => this._read(filename)); }
  getProject(projectId) { return this._project(projectId, (filename, relationship) => this._read(filename, relationship.projectId)); }

  async getEffective(projectId = null) {
    if (projectId === null || projectId === undefined) return { ...await this.getGlobal(), projectId: null, projectRevision: null,
      projectIndexing: { mountedFolder: false, bindingRevision: 0, knowledgeIds: [] } };
    return this._project(projectId, async (filename, relationship) => {
      const global = await this._read(join(this.root, 'Retrieval', 'settings.json'));
      const project = await this._read(filename, relationship.projectId);
      const sources = project.indexingSources;
      return { ...mergeSettings(global, project.overrides), projectId: relationship.projectId, projectRevision: project.revision,
        projectIndexing: { mountedFolder: sources.mountedFolder.enabled && !relationship.isFolderlessWorkspace,
          bindingRevision: sources.mountedFolder.bindingRevision, knowledgeIds: [...sources.knowledgeIds] } };
    });
  }

  _input(input, validate) {
    requireKeys(input, ['expectedRevision', 'patch'], 'settings mutation');
    integer(input.expectedRevision, 0, Number.MAX_SAFE_INTEGER - 1, 'expectedRevision');
    return { expectedRevision: input.expectedRevision, patch: validate(input.patch) };
  }

  patchGlobal(input) {
    const mutation = this._input(input, settingsPatch);
    return this._global(async filename => {
      const current = await this._read(filename);
      if (current.revision !== mutation.expectedRevision)
        throw retrievalFailure('Retrieval settings changed. / 检索配置已被修改，请刷新后重试。', 'RETRIEVAL_SETTINGS_CONFLICT', 409);
      const next = { ...mergeSettings(current, mutation.patch), revision: current.revision + 1 };
      await this._safe(filename, true);
      await atomicJson(filename, next);
      return next;
    });
  }

  patchProject(projectId, input) {
    const mutation = this._input(input, projectPatch);
    return this._project(projectId, async (filename, relationship) => {
      const current = await this._read(filename, relationship.projectId);
      if (current.revision !== mutation.expectedRevision)
        throw retrievalFailure('Project retrieval settings changed. / 工作检索配置已被修改，请刷新后重试。', 'RETRIEVAL_SETTINGS_CONFLICT', 409);
      if (relationship.isFolderlessWorkspace && mutation.patch.indexingSources?.mountedFolder?.enabled)
        throw retrievalFailure('No mounted folder is available. / 此工作没有挂载文件夹。', 'RETRIEVAL_FOLDER_UNAVAILABLE', 409);
      const next = { ...current, revision: current.revision + 1,
        overrides: Object.fromEntries(['local', 'web', 'cache'].filter(name => mutation.patch.overrides?.[name] !== null &&
          (current.overrides[name] || mutation.patch.overrides?.[name]))
          .map(name => [name, { ...current.overrides[name], ...mutation.patch.overrides?.[name] }])),
        indexingSources: { ...current.indexingSources, ...mutation.patch.indexingSources,
          mountedFolder: { ...current.indexingSources.mountedFolder, ...mutation.patch.indexingSources?.mountedFolder } } };
      if (current.indexingSources.mountedFolder.enabled !== next.indexingSources.mountedFolder.enabled)
        next.indexingSources.mountedFolder.bindingRevision++;
      await this._safe(filename, true);
      await atomicJson(filename, next);
      return next;
    });
  }
}
