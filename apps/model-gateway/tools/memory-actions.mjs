const boundedString = maximum => ({ type: 'string', minLength: 1, maxLength: maximum });
const scope = { type: 'string', enum: ['chat', 'project', 'user'] };

// Model tools expose proposals only; confirmed memory is changed by the existing user API.
// 模型工具只暴露建议，权威记忆仍通过现有用户确认 API 修改。
export const memoryActionDescriptors = [
  { name: 'memory.read', source: 'builtin', modelExposure: 'on-demand', description: 'Page current-chat drafts and confirmed visible memory. query is lexical; empty pages all. Shared entries omit private quotes. Use exact target/revisions for update/delete.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      scopes: { type: 'array', maxItems: 3, items: scope }, query: { type: 'string', maxLength: 2000 },
      offset: { type: 'integer', minimum: 0, maximum: 3000 },
      limit: { type: 'integer', minimum: 1, maximum: 50 } } } },
  { name: 'memory.propose', source: 'builtin', modelExposure: 'on-demand', description: 'Drafts need user confirmation and exact chat quotes. update/delete: unchanged memory.read target. noop: no writes.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['action', 'reason', 'isInference'], properties: {
      action: { type: 'string', enum: ['add', 'update', 'delete', 'noop'] }, scope,
      scopeId: boundedString(64), content: boundedString(4000), kind: { type: 'string', enum: ['fact', 'preference', 'decision'] },
      reason: boundedString(1000), isInference: { type: 'boolean' },
      conflicts: { type: 'array', maxItems: 8, items: boundedString(500) },
      quotes: { type: 'array', minItems: 1, maxItems: 12, items: { type: 'object', additionalProperties: false,
        required: ['messageId', 'text'], properties: { messageId: boundedString(64), text: boundedString(4000) } } },
      target: { type: 'object', additionalProperties: false, required: ['id', 'scope', 'scopeId', 'expectedRevision', 'entryRevision', 'identity'],
        properties: { id: boundedString(64), scope, scopeId: boundedString(64),
          expectedRevision: { type: 'integer', minimum: 0 }, entryRevision: { type: 'integer', minimum: 1 }, identity: boundedString(64) } } } } }
];
