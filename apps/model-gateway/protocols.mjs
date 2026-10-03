import { finalParts, checkFinish } from './streaming.mjs';

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

export function chatRequest(connection, model, messages, { stream = false, system = '', maxOutputTokens, tools } = {}) {
  const toolOptions = tools?.length ? { tools, tool_choice: connection.protocol === 'anthropic-messages' ? { type: 'auto' } : 'auto' } : {};
  const outputLimit = maxOutputTokens == null ? {} : { max_tokens: maxOutputTokens };
  switch (connection.protocol) {
    case 'anthropic-messages':
      return { path: '/messages', body: { model, messages, ...toolOptions, max_tokens: 8192, ...outputLimit, stream,
        ...(system ? { system } : {}) } };
    case 'openai-responses': {
      const official = new URL(connection.baseUrl).hostname === 'api.openai.com';
      const baseModel = model.replace(/-\d{4}-\d{2}-\d{2}$/, '');
      return { path: '/responses', body: { model, input: messages, store: false, ...toolOptions,
        ...(system ? { instructions: system } : {}),
        ...(maxOutputTokens == null ? {} : { max_output_tokens: maxOutputTokens }),
        stream: stream && !(official && nonStreamingResponseModels.has(baseModel)),
        ...(stream && official && summaryModels.has(baseModel) ? { reasoning: { summary: 'auto' } } : {}),
        ...(tools?.length && official && summaryModels.has(baseModel) ? { include: ['reasoning.encrypted_content'] } : {}) } };
    }
    default:
      return { path: '/chat/completions', body: { model,
        messages: system ? [{ role: 'system', content: system }, ...messages] : messages,
        stream, ...outputLimit, ...toolOptions } };
  }
}

export function responseText(protocol, result) {
  const parts = finalParts(protocol, result);
  try { checkFinish(parts.finish); }
  catch (error) { error.content = parts.content; error.reasoning = parts.reasoning; throw error; }
  return parts.content;
}
