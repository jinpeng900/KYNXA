const reason = { type: 'string', minLength: 1, maxLength: 2000 };
export const retrievalDescriptors = [
  { name: 'knowledge.search', source: 'builtin', description: 'Search authorized local documents and code. Optional domain, symbol and relative path prioritize indexed declarations or a file. These are bounded candidates, not exhaustive references or a call graph. Answer when evidence supports the facts; specify a concrete gap for additional searches. Prefer knowledge.read section or unit for omitted context. Other chats are not shared.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 12 },
      domain: { type: 'string', enum: ['knowledge', 'code', 'mixed'] },
      symbol: { type: 'string', minLength: 1, maxLength: 256, description: 'Exact declaration name or qualified name, for example SourceIndexer.upsert.' },
      path: { type: 'string', minLength: 1, maxLength: 1000, description: 'Relative file or directory path; this is an indexed locator, not filesystem permission.' },
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Specific unanswered fact or omitted condition, not a restatement of the entire task.' } } } },
  { name: 'knowledge.read', source: 'builtin', description: 'Read current authorized evidence. Use section for its chapter, unit for the indexed function/method or document block, or window for nearby text. Bounded unit reads report omitted parts and fall back honestly when structure is unavailable. Name the missing fact in gap. Page mode preserves pagination; changed/revoked sources require a new search.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['sourceRef'], properties: {
      sourceRef: { type: 'string', minLength: 1, maxLength: 4096 }, offset: { type: 'integer', minimum: 0, maximum: 2097152 },
      limit: { type: 'integer', minimum: 1, maximum: 16000, description: 'Maximum characters. Page mode accepts 1 or more; window, section and unit require at least 2 to preserve complete UTF-16 characters.' },
      mode: { type: 'string', enum: ['page', 'window', 'section', 'unit'] },
      anchorOffset: { type: 'integer', minimum: 0, maximum: 2097152 },
      beforeCharacters: { type: 'integer', minimum: 0, maximum: 4000 },
      gap: { type: 'string', minLength: 1, maxLength: 1000 } } } },
  { name: 'web.search', source: 'builtin', description: 'Search public websites using an already enabled Exa or Brave MCP provider. Returns concise sources, URLs and archived original-result reference. Honors per-project search mode and stage budget. Stop when evidence answers the question; use web.fetch for public page text before browser automation.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query', 'reason'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 12 }, reason,
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Concrete missing evidence that warrants another search. Do not search again when the requested facts already have sources.' } } } }
];
