import { randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, writeFile, unlink } from 'node:fs/promises';
import { resolve, join, dirname } from 'node:path';
import { isIP } from 'node:net';
import { protocols } from './protocols.mjs';

const providerPattern = /^[a-z][a-z0-9-]{1,39}$/;
const modelPattern = /^[^\s\x00-\x1f]{1,160}$/;
export const MAX_CONNECTION_MODELS = 10000;

export function isLocalEndpoint(url) {
  const host = url.hostname.replace(/^\[|\]$/g, '').toLowerCase();
  if (host === 'localhost' || host === '::1') return true;
  if (isIP(host) === 6) {
    // URL canonicalizes IPv4-mapped addresses into two hexadecimal groups.
    const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(host);
    if (mapped) {
      const high = parseInt(mapped[1], 16), low = parseInt(mapped[2], 16);
      return privateV4([high >> 8, high & 255, low >> 8, low & 255]);
    }
    return /^f[cd][0-9a-f]{2}:/.test(host);
  }
  return isIP(host) === 4 && privateV4(host.split('.').map(Number));
}

function privateV4([a, b]) {
  return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168);
}

export function validateConnection(input, { requireModels = true } = {}) {
  if (!input || typeof input !== 'object') throw new Error('连接信息不能为空。');
  const providerId = String(input.providerId ?? '').trim();
  const displayName = String(input.displayName ?? '').trim();
  const baseUrl = String(input.baseUrl ?? '').trim().replace(/\/+$/, '');
  const apiKey = input.apiKey == null ? undefined : String(input.apiKey);
  const protocol = input.protocol ?? 'openai-completions';
  const contextWindowTokens = input.contextWindowTokens;
  if (contextWindowTokens !== undefined && (!Number.isSafeInteger(contextWindowTokens) ||
      contextWindowTokens < 2048 || contextWindowTokens > 2000000))
    throw new Error('模型上下文预算须为 2048–2000000 的整数。');
  if (!protocols.includes(protocol)) throw new Error('模型接口协议无效。');
  const models = Array.isArray(input.models) ? [...new Set(input.models.map(value => String(value).trim()))] : [];
  if (!providerPattern.test(providerId)) throw new Error('Provider ID 须为 2–40 位小写字母、数字或连字符，且以字母开头。');
  if (!displayName || displayName.length > 80) throw new Error('连接名称须为 1–80 个字符。');
  let url;
  try { url = new URL(baseUrl); } catch { throw new Error('Base URL 格式无效。'); }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash)
    throw new Error('Base URL 只能是 HTTP(S) 地址，不能包含账号、查询参数或片段。');
  if (url.protocol !== 'https:' && !isLocalEndpoint(url))
    throw new Error('公网连接必须使用 HTTPS；本机或局域网 IP 可使用 HTTP。');
  if ((requireModels && !models.length) || models.length > MAX_CONNECTION_MODELS || models.some(model => !modelPattern.test(model)))
    throw new Error(`至少填写一个有效 Model ID，最多 ${MAX_CONNECTION_MODELS} 个。`);
  if (apiKey !== undefined && (apiKey.length > 8192 || /[\r\n]/.test(apiKey)))
    throw new Error('API Key 格式无效。');
  return { providerId, displayName, baseUrl, apiKey, models, protocol,
    ...(contextWindowTokens === undefined ? {} : { contextWindowTokens }) };
}

export async function readJson(filename, fallback) {
  try { return JSON.parse(await readFile(filename, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return fallback; throw error; }
}

export async function atomicJson(filename, value) {
  await mkdir(dirname(filename), { recursive: true, mode: 0o700 });
  const temp = `${filename}.${randomUUID()}.tmp`;
  try {
    await writeFile(temp, JSON.stringify(value, null, 2), { mode: 0o600, flag: 'wx' });
    await rename(temp, filename);
  } finally { await unlink(temp).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
}

function publicProvider({ apiKey, ...provider }) {
  return { ...provider, hasApiKey: Boolean(apiKey), protocol: provider.protocol ?? 'openai-completions' };
}

export class ModelStore {
  constructor({ dataHome }) {
    this.dataHome = resolve(dataHome);
    this.settingsPath = join(this.dataHome, 'connections.json');
    this.queue = Promise.resolve();
  }

  async document() {
    const document = await readJson(this.settingsPath, { version: 1, providers: [] });
    if (document?.version !== 1 || !Array.isArray(document.providers))
      throw new Error('模型配置文件无效，请从备份恢复。');
    return document;
  }

  async list() { return (await this.document()).providers.map(publicProvider); }

  async connectionFor(providerId) {
    return (await this.document()).providers.find(provider => provider.providerId === providerId);
  }

  async savedKeyFor(providerId, baseUrl) {
    const provider = await this.connectionFor(providerId);
    return provider?.baseUrl === baseUrl ? provider.apiKey : undefined;
  }

  async save(input) {
    const connection = validateConnection(input);
    const operation = this.queue.then(async () => {
      const document = await this.document();
      const previous = document.providers.find(provider => provider.providerId === connection.providerId);
      // Never carry a saved key across endpoints.
      const apiKey = connection.apiKey || (previous?.baseUrl === connection.baseUrl ? previous.apiKey : undefined);
      if (!apiKey && !isLocalEndpoint(new URL(connection.baseUrl)))
        throw new Error('此连接需要 API Key。');
      const provider = { ...connection, apiKey,
        ...(connection.contextWindowTokens === undefined && previous?.contextWindowTokens !== undefined
          ? { contextWindowTokens: previous.contextWindowTokens } : {}) };
      document.providers = document.providers.filter(item => item.providerId !== connection.providerId);
      document.providers.push(provider);
      await atomicJson(this.settingsPath, document);
      return publicProvider(provider);
    });
    this.queue = operation.catch(() => {});
    return operation;
  }
}
