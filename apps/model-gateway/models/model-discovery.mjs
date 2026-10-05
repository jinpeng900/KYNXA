import { authorization } from './protocols.mjs';
import { MAX_CONNECTION_MODELS } from './store.mjs';

const modelPattern = /^[^\s\x00-\x1f]{1,160}$/;
const maximumPages = 100;
const maximumPageBytes = 8 * 1024 * 1024;

async function readModelPage(response) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of response.body) {
    bytes += chunk.length;
    if (bytes > maximumPageBytes) throw new Error('模型列表响应过大，未返回不完整的列表。');
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
  catch { throw new Error('模型列表接口未返回有效 JSON。'); }
}

// Anthropic documents cursor pagination separately from its Messages API.
// OpenAI-compatible endpoints normally return one complete list; do not guess
// an undocumented cursor or follow a provider-supplied URL with credentials.
// Anthropic 的游标分页文档与 Messages API 分开。
export async function discoverModels(connection) {
  const endpoint = new URL(`${connection.baseUrl}/models`);
  const anthropic = connection.protocol === 'anthropic-messages';
  if (anthropic) endpoint.searchParams.set('limit', '1000');
  const models = new Set();
  const cursors = new Set();
  const signal = AbortSignal.timeout(30000);
  for (let page = 0; page < maximumPages; page++) {
    const response = await fetch(endpoint, {
      redirect: 'error', headers: authorization(connection), signal
    });
    if (!response.ok) {
      await response.body?.cancel();
      throw new Error(`模型列表接口返回 HTTP ${response.status}。`);
    }
    const result = await readModelPage(response);
    const items = Array.isArray(result?.data) ? result.data
      : Array.isArray(result?.models) ? result.models : null;
    if (!items) throw new Error('模型列表接口返回格式无效。');
    for (const item of items) {
      const id = item?.id ?? item?.name;
      if (typeof id !== 'string' || !modelPattern.test(id))
        throw new Error('模型列表包含无效 Model ID，未返回不完整的列表。');
      models.add(id);
      if (models.size > MAX_CONNECTION_MODELS)
        throw new Error(`模型列表超过 ${MAX_CONNECTION_MODELS} 个，未返回不完整的列表。`);
    }
    if ((result.has_more != null && typeof result.has_more !== 'boolean') ||
        result.next_page_token || result.nextPageToken || result.next_page)
      throw new Error('模型列表返回了不支持的分页格式，未返回不完整的列表。');
    if (result.has_more !== true) return [...models];
    if (!anthropic)
      throw new Error('此模型列表的分页协议尚不支持，未返回不完整的列表。');
    const cursor = result.last_id;
    if (items.length === 0 || typeof cursor !== 'string' || !modelPattern.test(cursor) || cursors.has(cursor))
      throw new Error('模型列表分页游标无效或重复，未返回不完整的列表。');
    cursors.add(cursor);
    endpoint.searchParams.set('after_id', cursor);
  }
  throw new Error('模型列表分页超过限制，未返回不完整的列表。');
}
