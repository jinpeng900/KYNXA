export const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];

// Opt in only for known Responses reasoning models on the official endpoint.
// In particular, chat-latest aliases and o3-mini must not inherit this option.
const summaryModels = new Set(['o3', 'o3-pro', 'o4-mini', 'gpt-5', 'gpt-5-mini', 'gpt-5-nano', 'gpt-5-pro',
  'gpt-5.1', 'gpt-5.2', 'gpt-5.2-pro', 'gpt-5.4', 'gpt-5.4-mini', 'gpt-5.4-nano', 'gpt-5.4-pro',
  'gpt-5.3-codex', 'gpt-5.5', 'gpt-5.5-pro', 'gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna',
  'gpt-6-astra', 'gpt-6.1-sol', 'gpt-6-sol', 'gpt-6-luna']);

// Official model pages mark these Responses models as non-streaming (2026-10-01).
// The runtime already forwards complete JSON responses through the same UI event protocol.
const nonStreamingResponseModels = new Set(['gpt-5.5-pro', 'o3-pro']);

export function authorization(connection) {
  if (connection.protocol === 'anthropic-messages')
    return { 'anthropic-version': '2023-06-01', ...(connection.apiKey ? { 'x-api-key': connection.apiKey } : {}) };
  return connection.apiKey ? { Authorization: `Bearer ${connection.apiKey}` } : {};
}

export function chatRequest(connection, model, messages, { stream = false } = {}) {
  switch (connection.protocol) {
    case 'anthropic-messages':
      return { path: '/messages', body: { model, messages, max_tokens: 8192, stream } };
    case 'openai-responses': {
      const official = new URL(connection.baseUrl).hostname === 'api.openai.com';
      const baseModel = model.replace(/-\d{4}-\d{2}-\d{2}$/, '');
      return { path: '/responses', body: { model, input: messages, store: false,
        stream: stream && !(official && nonStreamingResponseModels.has(baseModel)),
        ...(stream && official && summaryModels.has(baseModel) ? { reasoning: { summary: 'auto' } } : {}) } };
    }
    default:
      return { path: '/chat/completions', body: { model, messages, stream } };
  }
}

export function responseText(protocol, result) {
  if (protocol === 'anthropic-messages')
    return result.content?.filter(block => block.type === 'text').map(block => block.text).join('\n');
  if (protocol === 'openai-responses') {
    if (result.status === 'incomplete' || result.status === 'failed') throw new Error('模型未完成回复，请重试。');
    return result.output?.filter(item => item.type === 'message')
      .flatMap(item => item.content ?? []).filter(block => block.type === 'output_text')
      .map(block => block.text).join('\n');
  }
  return result.choices?.[0]?.message?.content;
}
