export const protocols = ['openai-completions', 'openai-responses', 'anthropic-messages'];

export function authorization(connection) {
  if (connection.protocol === 'anthropic-messages')
    return { 'anthropic-version': '2023-06-01', ...(connection.apiKey ? { 'x-api-key': connection.apiKey } : {}) };
  return connection.apiKey ? { Authorization: `Bearer ${connection.apiKey}` } : {};
}

export function chatRequest(connection, model, messages) {
  switch (connection.protocol) {
    case 'anthropic-messages':
      return { path: '/messages', body: { model, messages, max_tokens: 8192, stream: false } };
    case 'openai-responses':
      return { path: '/responses', body: { model, input: messages, store: false, stream: false } };
    default:
      return { path: '/chat/completions', body: { model, messages, stream: false } };
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
