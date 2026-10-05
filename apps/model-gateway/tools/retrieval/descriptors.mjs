const reason = { type: 'string', minLength: 1, maxLength: 2000 };
export const retrievalDescriptors = [
  { name: 'knowledge.search', source: 'builtin', description: 'Search authorized local knowledge. Answer directly when current excerpts support the requested facts. For additional searches specify the concrete missing fact in gap; prefer knowledge.read for a missing section of an existing source. Relevance scores do not prove sufficiency. Other chats are not shared.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 12 },
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Specific unanswered fact or omitted condition, not a restatement of the entire task.' } } } },
  { name: 'knowledge.read', source: 'builtin', description: 'Read current authorized evidence. Use mode section for the matching chapter or window for nearby text, with gap naming the missing fact. These modes default to the referenced chunk, not the document start. Page mode preserves offset pagination. Short sourceRefs survive restart; changed/revoked sources require a new search.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['sourceRef'], properties: {
      sourceRef: { type: 'string', minLength: 1, maxLength: 4096 }, offset: { type: 'integer', minimum: 0, maximum: 2097152 },
      limit: { type: 'integer', minimum: 1, maximum: 16000 },
      mode: { type: 'string', enum: ['page', 'window', 'section'] },
      anchorOffset: { type: 'integer', minimum: 0, maximum: 2097152 },
      beforeCharacters: { type: 'integer', minimum: 0, maximum: 4000 },
      gap: { type: 'string', minLength: 1, maxLength: 1000 } } } },
  { name: 'web.search', source: 'builtin', description: 'Search public websites using an already enabled Exa or Brave MCP provider. Returns concise sources, URLs and archived original-result reference. Honors per-project search mode and stage budget. Stop when evidence answers the question; use web.fetch for public page text before browser automation.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query', 'reason'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 12 }, reason,
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Concrete missing evidence that warrants another search. Do not search again when the requested facts already have sources.' } } } }
];
