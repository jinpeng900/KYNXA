import { parseDocument, isAlias, isMap, isSeq, isScalar } from 'yaml';

const MAX_HEADER_BYTES = 32768;
const MAX_NODES = 1024;
const standardTags = new Set(['tag:yaml.org,2002:str', 'tag:yaml.org,2002:null',
  'tag:yaml.org,2002:bool', 'tag:yaml.org,2002:int', 'tag:yaml.org,2002:float',
  'tag:yaml.org,2002:map', 'tag:yaml.org,2002:seq']);
const standardFields = new Set(['name', 'description', 'license', 'compatibility', 'metadata', 'allowed-tools']);

/** Frontmatter is data, never executable YAML. Bound the AST before conversion. */
function frontmatterValues(content) {
  if (typeof content !== 'string') return null;
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(content.replace(/^\uFEFF/, ''));
  if (!match || Buffer.byteLength(match[1]) > MAX_HEADER_BYTES) return null;
  try {
    const document = parseDocument(match[1], { version: '1.2', schema: 'core', strict: true,
      uniqueKeys: true, customTags: [], merge: false });
    if (document.errors.length || document.warnings.length || !isMap(document.contents)) return null;
    let nodes = 0;
    const convert = (node, depth = 0) => {
      if (++nodes > MAX_NODES || depth > 12 || isAlias(node) ||
          (node?.tag && !standardTags.has(node.tag))) throw new Error('Restricted YAML');
      if (isMap(node)) {
        const entries = node.items.map(pair => {
          const key = convert(pair.key, depth + 1);
          if (typeof key !== 'string' || ['__proto__', 'prototype', 'constructor', '<<'].includes(key))
            throw new Error('Invalid metadata key');
          return [key, convert(pair.value, depth + 1)];
        });
        return Object.fromEntries(entries);
      }
      if (isSeq(node)) return node.items.map(item => convert(item, depth + 1));
      if (node == null) return null;
      if (!isScalar(node) || (typeof node.value === 'number' && !Number.isFinite(node.value)))
        throw new Error('Invalid metadata value');
      return node.value;
    };
    return convert(document.contents);
  } catch { return null; }
}

/** Preserve readable legacy metadata; standard validation is a separate import boundary. */
export function parseSkillFrontmatter(content) {
  const values = frontmatterValues(content);
  if (!values) return null;
  if (typeof values.name !== 'string' || !values.name.trim() || values.name.length > 100 ||
      typeof values.description !== 'string' || !values.description.trim() || values.description.length > 2000) return null;
  for (const field of ['license', 'compatibility'])
    if (values[field] != null && (typeof values[field] !== 'string' || values[field].length > 2000)) return null;
  if (values.metadata != null && (!values.metadata || typeof values.metadata !== 'object' || Array.isArray(values.metadata))) return null;
  const allowedTools = values['allowed-tools'];
  if (allowedTools != null && typeof allowedTools !== 'string' &&
      !(Array.isArray(allowedTools) && allowedTools.every(value => typeof value === 'string'))) return null;
  const extra = Object.fromEntries(Object.entries(values).filter(([key]) => !['name', 'description'].includes(key)));
  return { name: values.name.trim(), description: values.description.trim(),
    ...(Object.keys(extra).length ? { metadata: extra } : {}) };
}

/** Agent Skills format diagnostics do not silently rewrite existing skill files. */
export function validateSkillFrontmatter(content, { directoryName } = {}) {
  const values = frontmatterValues(content), header = parseSkillFrontmatter(content), diagnostics = [];
  const error = (code, field, message) => diagnostics.push({ code, field, severity: 'error', message });
  const length = value => Array.from(value).length;
  if (!values) {
    error('SKILL_FRONTMATTER_INVALID', 'frontmatter', 'Skill frontmatter must be a bounded, safe YAML mapping.');
    return { valid: false, header: null, diagnostics };
  }
  const name = typeof values.name === 'string' ? values.name.trim().normalize('NFKC') : '';
  if (!name || length(name) > 64)
    error('SKILL_NAME_LENGTH', 'name', 'Skill name must contain 1–64 characters.');
  if (name && (name !== name.toLowerCase() || !/^[\p{L}\p{N}-]+$/u.test(name) ||
      name.startsWith('-') || name.endsWith('-') || name.includes('--')))
    error('SKILL_NAME_FORMAT', 'name', 'Skill name must use lowercase letters, numbers and single internal hyphens.');
  if (name && directoryName != null && directoryName.normalize('NFKC') !== name)
    error('SKILL_DIRECTORY_NAME', 'name', 'The parent directory must match the skill name.');
  if (typeof values.description !== 'string' || !values.description.trim() || length(values.description) > 1024)
    error('SKILL_DESCRIPTION_LENGTH', 'description', 'Skill description must contain 1–1024 characters.');
  if (Object.hasOwn(values, 'compatibility') && (typeof values.compatibility !== 'string' ||
      !values.compatibility.trim() || length(values.compatibility) > 500))
    error('SKILL_COMPATIBILITY_LENGTH', 'compatibility', 'Compatibility, when provided, must contain 1–500 characters.');
  if (Object.hasOwn(values, 'license') && typeof values.license !== 'string')
    error('SKILL_LICENSE_FORMAT', 'license', 'License must be a string.');
  if (Object.hasOwn(values, 'metadata') && (!values.metadata || typeof values.metadata !== 'object' ||
      Array.isArray(values.metadata) || Object.values(values.metadata).some(value => typeof value !== 'string')))
    error('SKILL_METADATA_FORMAT', 'metadata', 'Metadata must map string keys to string values.');
  if (Object.hasOwn(values, 'allowed-tools') && typeof values['allowed-tools'] !== 'string')
    error('SKILL_ALLOWED_TOOLS_FORMAT', 'allowed-tools', 'Allowed tools must be a space-separated string.');
  for (const field of Object.keys(values)) if (!standardFields.has(field))
    error('SKILL_UNKNOWN_FIELD', field, 'Put additional frontmatter fields inside metadata.');
  if (!header && !diagnostics.length)
    error('SKILL_FRONTMATTER_LIMIT', 'frontmatter', 'Skill metadata exceeds the application safety limits.');
  return { valid: diagnostics.length === 0, header, diagnostics };
}
