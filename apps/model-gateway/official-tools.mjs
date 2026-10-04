import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export { curatedMcpPresets } from './official-tools/MCP/catalog.mjs';

export const OFFICIAL_TOOLS_PACKAGE_ID = 'kynxa-official-tools';
export const OFFICIAL_TOOLS_ROOT = fileURLToPath(new URL('./official-tools/', import.meta.url));
export const OFFICIAL_SKILLS_DIRECTORY = fileURLToPath(new URL('./official-tools/Skills/', import.meta.url));
export const LEGACY_BUNDLED_SKILLS_DIRECTORY = fileURLToPath(new URL('./skills/', import.meta.url));
const manifestUrl = new URL('./official-tools/manifest.json', import.meta.url);
const shortHash = value => createHash('sha256').update(value, 'utf8').digest('hex').slice(0, 24);
let manifestPromise;

export async function readOfficialToolsManifest() {
  // Installed metadata is read-only. Return a copy so callers cannot change the shared package inventory.
  // 安装元信息只读，返回副本避免调用方修改共享包清单。
  manifestPromise ??= readFile(manifestUrl, 'utf8').then(text => {
    const manifest = JSON.parse(text);
    if (manifest.schemaVersion !== 1 || manifest.packageId !== OFFICIAL_TOOLS_PACKAGE_ID ||
        typeof manifest.version !== 'string' || !Array.isArray(manifest.skills) ||
        !Array.isArray(manifest.coreTools) || !Array.isArray(manifest.mcpPresets))
      throw new Error('Invalid official tools package manifest.');
    return manifest;
  }).catch(error => { manifestPromise = undefined; throw error; });
  return structuredClone(await manifestPromise);
}

export function officialSkillIdentity(relativePath, {
  skillsDirectory = OFFICIAL_SKILLS_DIRECTORY, legacySkillsDirectory = LEGACY_BUNDLED_SKILLS_DIRECTORY
} = {}) {
  const path = typeof relativePath === 'string' ? relativePath.replaceAll('\\', '/') : '';
  const parts = path.split('/');
  if (!path || isAbsolute(path) || parts.some(part => !part || part === '.' || part === '..' || /[:\0]/u.test(part)) ||
      parts.at(-1) !== 'SKILL.md') throw new TypeError('Invalid official skill relative path.');
  const id = shortHash(`${OFFICIAL_TOOLS_PACKAGE_ID}/skills/${path}`);
  const legacyIds = [...new Set([shortHash(resolve(legacySkillsDirectory, ...parts)),
    shortHash(resolve(skillsDirectory, ...parts))])].filter(value => value !== id);
  return { id, legacyIds };
}

export async function normalizeOfficialDisabledSkills(ids) {
  const manifest = await readOfficialToolsManifest(), aliases = new Map();
  for (const skill of manifest.skills) {
    const { id, legacyIds } = officialSkillIdentity(skill.path);
    for (const alias of legacyIds) aliases.set(alias, id);
  }
  return [...new Set((ids ?? []).map(id => aliases.get(id) ?? id))];
}
