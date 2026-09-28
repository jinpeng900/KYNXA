import { createRequire } from 'node:module';
import { randomUUID } from 'node:crypto';
import { chmod, mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';

const providerPattern = /^[a-z][a-z0-9-]{1,39}$/;
const modelPattern = /^[^\s\x00-\x1f]{1,160}$/;

export function validateConnection(input, { requireModels = true } = {}) {
  if (!input || typeof input !== 'object') throw new Error('连接信息不能为空。');
  const providerId = String(input.providerId ?? '').trim();
  const displayName = String(input.displayName ?? '').trim();
  const baseUrl = String(input.baseUrl ?? '').trim().replace(/\/+$/, '');
  const apiKey = input.apiKey == null ? undefined : String(input.apiKey);
  const models = Array.isArray(input.models) ? [...new Set(input.models.map(value => String(value).trim()))] : [];
  if (!providerPattern.test(providerId)) throw new Error('Provider ID 须为 2–40 位小写字母、数字或连字符，且以字母开头。');
  if (!displayName || displayName.length > 80) throw new Error('连接名称须为 1–80 个字符。');
  let url;
  try { url = new URL(baseUrl); } catch { throw new Error('Base URL 格式无效。'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new Error('Base URL 只能是 HTTP(S) 地址，不能包含账号、查询参数或片段。');
  if (url.protocol !== 'https:' && !['localhost', '127.0.0.1', '[::1]'].includes(url.hostname))
    throw new Error('非本机连接必须使用 HTTPS。');
  if ((requireModels && !models.length) || models.length > 100 || models.some(model => !modelPattern.test(model)))
    throw new Error('至少填写一个有效 Model ID，最多 100 个。');
  if (apiKey !== undefined && (apiKey.length > 8192 || /[\r\n]/.test(apiKey)))
    throw new Error('API Key 格式无效。');
  return { providerId, displayName, baseUrl, apiKey, models };
}

export function credentialRef(providerId) {
  return `KYNXA_${providerId.toUpperCase().replaceAll('-', '_')}_API_KEY`;
}

function yamlFor(harnessRoot) {
  const require = createRequire(join(resolve(harnessRoot), 'apps/cli/package.json'));
  return require('js-yaml');
}

async function readDocument(filename, yaml) {
  let content;
  try { content = await readFile(filename, 'utf8'); }
  catch (error) { if (error.code === 'ENOENT') return {}; throw error; }
  const value = yaml.load(content);
  if (value === undefined || value === null) return {};
  if (typeof value !== 'object' || Array.isArray(value)) throw new Error(`${filename} 不是有效的配置对象。`);
  return value;
}

async function atomicDocument(filename, value, yaml) {
  const temp = `${filename}.${randomUUID()}.tmp`;
  await writeFile(temp, yaml.dump(value, { noRefs: true, lineWidth: 120 }), { mode: 0o600 });
  await rename(temp, filename);
  await chmod(filename, 0o600);
}

export class HarnessModelStore {
  constructor({ harnessRoot, dataHome }) {
    this.yaml = yamlFor(harnessRoot);
    this.dataHome = resolve(dataHome);
    this.settingsPath = join(this.dataHome, 'settings.yaml');
    this.credentialsPath = join(this.dataHome, '.credentials.yaml');
    this.queue = Promise.resolve();
  }

  async list() {
    const settings = await readDocument(this.settingsPath, this.yaml);
    const credentials = await readDocument(this.credentialsPath, this.yaml);
    const profiles = settings['llm-pi-ai']?.providers ?? {};
    return Object.entries(profiles).map(([providerId, profile]) => ({
      providerId,
      displayName: profile.displayName ?? providerId,
      baseUrl: profile.baseURL ?? '',
      models: (profile.models ?? []).map(model => model.id),
      hasApiKey: Boolean(credentials.refs?.[credentialRef(providerId)]),
      protocol: profile.api ?? 'openai-completions'
    }));
  }

  async savedKeyFor(providerId, baseUrl) {
    const settings = await readDocument(this.settingsPath, this.yaml);
    const profile = settings['llm-pi-ai']?.providers?.[providerId];
    if (!profile || profile.baseURL !== baseUrl) return undefined;
    const credentials = await readDocument(this.credentialsPath, this.yaml);
    return credentials.refs?.[credentialRef(providerId)];
  }

  async save(input) {
    const connection = validateConnection(input);
    const operation = this.queue.then(async () => {
      await mkdir(this.dataHome, { recursive: true, mode: 0o700 });
      const settings = await readDocument(this.settingsPath, this.yaml);
      const credentials = await readDocument(this.credentialsPath, this.yaml);
      const profiles = { ...(settings['llm-pi-ai']?.providers ?? {}) };
      const refs = { ...(credentials.refs ?? {}) };
      const ref = credentialRef(connection.providerId);
      if (connection.apiKey !== undefined && connection.apiKey !== '') refs[ref] = connection.apiKey;
      // Keyless loopback endpoints use a local placeholder because the pi-ai OpenAI
      // Completions transport requires an Authorization value even for Ollama.
      if (!refs[ref] && new URL(connection.baseUrl).protocol === 'http:') refs[ref] = 'local';
      if (!refs[ref]) throw new Error('此连接需要 API Key。');
      profiles[connection.providerId] = {
        displayName: connection.displayName,
        api: 'openai-completions',
        baseURL: connection.baseUrl,
        apiKeyEnv: ref,
        compat: { supportsDeveloperRole: false, maxTokensField: 'max_tokens' },
        models: connection.models.map(id => ({ id }))
      };
      settings['llm-pi-ai'] = { ...(settings['llm-pi-ai'] ?? {}), providers: profiles };
      credentials.version = 1;
      credentials.refs = refs;
      credentials.records ??= {};
      await atomicDocument(this.credentialsPath, credentials, this.yaml);
      await atomicDocument(this.settingsPath, settings, this.yaml);
      return { providerId: connection.providerId, displayName: connection.displayName,
        baseUrl: connection.baseUrl, models: connection.models, hasApiKey: true,
        protocol: 'openai-completions' };
    });
    this.queue = operation.catch(() => {});
    return operation;
  }
}
