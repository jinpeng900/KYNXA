import { createHash } from 'node:crypto';
import { opendir, readFile } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inspectLocalPath, toolFailure } from './tool-paths.mjs';

const MAX_SKILL_BYTES = 256 * 1024;
const MAX_SKILLS = 128;
const MAX_CANDIDATES_PER_DIRECTORY = 512;
const decoder = new TextDecoder('utf-8', { fatal: true });
const defaultBundledDirectory = join(dirname(fileURLToPath(import.meta.url)), 'skills');

function metadata(content) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(content.replace(/^\uFEFF/, ''));
  if (!match) return null;
  const values = {}, lines = match[1].split(/\r?\n/);
  for (let index = 0; index < lines.length; index++) {
    const field = /^(name|description):\s*(.*?)\s*$/.exec(lines[index]);
    if (!field) continue;
    let value = field[2];
    if (['>', '|', '>-' , '|-'].includes(value)) {
      const parts = [];
      while (index + 1 < lines.length && /^\s+/.test(lines[index + 1])) parts.push(lines[++index].trim());
      value = parts.join(field[2].startsWith('|') ? '\n' : ' ');
    } else if (value.startsWith('"')) {
      try { value = JSON.parse(value); } catch { return null; }
    } else if (value.startsWith("'") && value.endsWith("'")) value = value.slice(1, -1).replace(/''/g, "'");
    else value = value.replace(/\s+#.*$/, '');
    values[field[1]] = value;
  }
  if (typeof values.name !== 'string' || !values.name.trim() || values.name.length > 100 ||
      typeof values.description !== 'string' || !values.description.trim() || values.description.length > 2000) return null;
  return { name: values.name.trim(), description: values.description.trim() };
}

export class AppSkillService {
  constructor(root, { bundledDirectory = defaultBundledDirectory } = {}) {
    this.root = resolve(root);
    this.bundledDirectory = bundledDirectory === null ? null : resolve(bundledDirectory);
    this.discovery = new WeakMap();
  }

  roots(context, config) {
    return [...new Set([...(this.bundledDirectory ? [this.bundledDirectory] : []), join(this.root, 'Skills'), ...config.skillDirectories,
      ...(context?.workspaceRoot ? [join(context.workspaceRoot, '.kynxa', 'skills')] : [])].map(path => resolve(path)))];
  }

  async _load(file, { includeContent = false } = {}) {
    const info = await inspectLocalPath(file, { allowMissing: true });
    if (!info) return null;
    if (!info.isFile() || info.size > MAX_SKILL_BYTES) throw toolFailure('应用技能文件过大或结构无效。', 'INVALID_APP_SKILL');
    const bytes = await readFile(file);
    if (bytes.length > MAX_SKILL_BYTES) throw toolFailure('应用技能文件过大。', 'INVALID_APP_SKILL');
    let content;
    try { content = decoder.decode(bytes); } catch { throw toolFailure('应用技能须使用 UTF-8 文本。', 'INVALID_APP_SKILL'); }
    if (content.includes('\0')) throw toolFailure('应用技能不能包含二进制文本。', 'INVALID_APP_SKILL');
    const header = metadata(content);
    if (!header) throw toolFailure('应用技能元数据无效，原文件已保留。', 'INVALID_APP_SKILL');
    const id = createHash('sha256').update(resolve(file)).digest('hex').slice(0, 24);
    return { id, ...header, source: resolve(file), ...(includeContent ? { content } : {}) };
  }

  async list(context, config) {
    const skills = [], ids = new Set();
    const status = { maxSkills: MAX_SKILLS, maxCandidatesPerDirectory: MAX_CANDIDATES_PER_DIRECTORY, truncated: false, unavailableCount: 0 };
    const add = skill => {
      if (skill && !ids.has(skill.id)) { ids.add(skill.id); skills.push(skill); }
    };
    for (const root of this.roots(context, config)) {
      if (skills.length >= MAX_SKILLS) { status.truncated = true; break; }
      let candidates;
      try {
        const info = await inspectLocalPath(root, { allowMissing: true });
        if (!info) continue;
        if (!info.isDirectory()) throw toolFailure('应用技能路径不是目录。', 'INVALID_APP_SKILL');
        candidates = [join(root, 'SKILL.md')];
        let visited = 0;
        for await (const entry of await opendir(root)) {
          if (visited++ >= MAX_CANDIDATES_PER_DIRECTORY) { status.truncated = true; break; }
          if (!entry.isSymbolicLink() && entry.isDirectory()) candidates.push(join(root, entry.name, 'SKILL.md'));
        }
        candidates.sort((left, right) => left.localeCompare(right));
      } catch (error) {
        status.unavailableCount++;
        add({ id: createHash('sha256').update(join(root, 'SKILL.md')).digest('hex').slice(0, 24), name: basename(root).slice(0, 100) || 'Unavailable skill directory',
          description: `Application skill directory unavailable (${error.code ?? 'APP_SKILL_UNAVAILABLE'}); original files preserved.`, source: join(root, 'SKILL.md'), status: 'unavailable' });
        continue;
      }
      for (const file of candidates) {
        if (skills.length >= MAX_SKILLS) { status.truncated = true; break; }
        try { add(await this._load(file)); }
        catch (error) {
          status.unavailableCount++;
          add({ id: createHash('sha256').update(resolve(file)).digest('hex').slice(0, 24), name: basename(dirname(file)).slice(0, 100) || 'Unavailable application skill',
            description: `Application skill unavailable (${error.code ?? 'APP_SKILL_UNAVAILABLE'}); original file preserved.`, source: resolve(file), status: 'unavailable' });
        }
      }
    }
    this.discovery.set(skills, status);
    return skills;
  }

  async read(id, context, config) {
    if (typeof id !== 'string' || !/^[a-f0-9]{24}$/.test(id)) throw toolFailure('应用技能 ID 无效。', 'APP_SKILL_NOT_FOUND', 404);
    const available = await this.list(context, config), source = available.find(skill => skill.id === id);
    if (!source) throw toolFailure('应用技能不存在或不属于当前范围。', 'APP_SKILL_NOT_FOUND', 404);
    if (source.status === 'unavailable') throw toolFailure(source.description, 'INVALID_APP_SKILL');
    const skill = await this._load(source.source, { includeContent: true });
    if (!skill || skill.id !== id) throw toolFailure('应用技能已变化，请重新读取。', 'APP_SKILL_NOT_FOUND', 404);
    return skill;
  }
}
