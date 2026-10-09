const reason = { type: 'string', minLength: 1, maxLength: 2000 };
export const retrievalDescriptors = [
  { name: 'knowledge.search', source: 'builtin', description: 'Search authorized local documents and code. domain is an explicit scope; choose it from the user or verified source, not words like method/function or brand capitalization. Keep ambiguous queries mixed. symbol/path need an explicit identifier or known relative locator; OpenAI alone is not a code symbol. Bounded candidates are not exhaustive references/call graphs; no hits under incomplete indexing mean only current coverage. Read omitted sections with knowledge.read. Other chats are not shared.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 48 },
      taskType: { type: 'string', enum: ['lookup', 'file', 'recall', 'complex', 'research'] },
      maximumTokens: { type: 'integer', minimum: 0, maximum: 32768, description: 'Evidence tokens only, bounded separately from the candidate pool and model context.' },
      domain: { type: 'string', enum: ['knowledge', 'code', 'mixed'], description: 'Explicit scope only; leave mixed when domain is uncertain. Lexical hints must not exclude other authorized evidence.' },
      symbol: { type: 'string', minLength: 1, maxLength: 256, description: 'Explicit exact declaration or qualified name, for example SourceIndexer.upsert; capitalization of a brand is insufficient.' },
      path: { type: 'string', minLength: 1, maxLength: 1000, description: 'Relative file or directory path; this is an indexed locator, not filesystem permission.' },
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Specific unanswered fact or omitted condition, not a restatement of the entire task.' } } } },
  { name: 'knowledge.read', source: 'builtin', description: 'Read current authorized evidence using a sourceRef returned by retrieval; never invent a reference or reuse a stale version. Use section for its chapter, unit for an indexed function/method or document block, or window for nearby text. Bounded reads report omitted parts and fall back when structure is unavailable. Name the missing fact in gap. Changed/revoked sources require a new search; incomplete coverage is not source absence.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['sourceRef'], properties: {
      sourceRef: { type: 'string', minLength: 1, maxLength: 4096 }, offset: { type: 'integer', minimum: 0, maximum: 2097152 },
      limit: { type: 'integer', minimum: 1, maximum: 16000, description: 'Maximum characters. Page mode accepts 1 or more; window, section and unit require at least 2 to preserve complete UTF-16 characters.' },
      mode: { type: 'string', enum: ['page', 'window', 'section', 'unit'] },
      anchorOffset: { type: 'integer', minimum: 0, maximum: 2097152 },
      beforeCharacters: { type: 'integer', minimum: 0, maximum: 4000 },
      gap: { type: 'string', minLength: 1, maxLength: 1000 } } } },
  { name: 'knowledge.relations', source: 'builtin', description: 'Navigate current authorized definitions, syntax ownership, document sections or textual symbol mentions using an existing sourceRef or explicit symbol. A brand name or a word like method alone does not prove a symbol. Lexical references are uncertain and non-exhaustive, not a language-service call graph. Read each related source before drawing conclusions.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      sourceRef: { type: 'string', minLength: 1, maxLength: 4096 }, symbol: { type: 'string', minLength: 1, maxLength: 256 },
      kind: { type: 'string', enum: ['definition', 'contains', 'section', 'lexical-reference'] },
      limit: { type: 'integer', minimum: 1, maximum: 100 } } } },
  { name: 'knowledge.assess', source: 'builtin', description: 'Record explicit claims, exact supporting quotations from evidence already read, unresolved gaps and conflicting sources. Revalidates all source versions and invalidates stale dependencies. Optional test checks require real current tool receipts; a model claim cannot certify tests. Stop searching when cited support is sufficient. Keeps a version-bound navigation experience only in this chat.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['claims'], properties: {
      conclusionId: { type: 'string', minLength: 1, maxLength: 128 },
      claims: { type: 'array', minItems: 1, maxItems: 32, items: { type: 'object', additionalProperties: false,
        required: ['statement', 'support'], properties: { statement: { type: 'string', minLength: 1, maxLength: 2000 },
          support: { type: 'array', maxItems: 16, items: { type: 'object', additionalProperties: false,
            required: ['sourceRef', 'quote'], properties: { sourceRef: { type: 'string', minLength: 1, maxLength: 4096 },
              quote: { type: 'string', minLength: 1, maxLength: 8000 } } } } } } },
      unresolved: { type: 'array', maxItems: 32, items: { type: 'string', maxLength: 1000 } },
      contradictions: { type: 'array', maxItems: 32, items: { type: 'string', maxLength: 1000 } },
      verification: { type: 'array', maxItems: 32, items: { type: 'object', additionalProperties: false,
        required: ['name', 'toolCallId'], properties: { name: { type: 'string', minLength: 1, maxLength: 200 },
          toolCallId: { type: 'string', minLength: 1, maxLength: 200 } } } } } } },
  { name: 'knowledge.experience', source: 'builtin', description: 'Recall current-chat navigation experience with live citation version checks. Historical answers are not current facts. Re-read and validate sources before reusing an experience; stale dependencies require repair.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      query: { type: 'string', maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 20 } } } },
  { name: 'web.search', source: 'builtin', description: 'Search public websites using an already enabled Exa or Brave MCP provider. Preserve the requested entities, dates and scope; add disambiguating terms only from current evidence, never an assumed year. Returns concise sources, URLs and archived original-result reference. Honors per-project search mode and stage budget. Stop when evidence answers the question; use web.fetch for public page text before browser automation.',
    inputSchema: { type: 'object', additionalProperties: false, required: ['query', 'reason'], properties: {
      query: { type: 'string', minLength: 1, maxLength: 2000 }, limit: { type: 'integer', minimum: 1, maximum: 12 }, reason,
      gap: { type: 'string', minLength: 1, maxLength: 1000, description: 'Concrete missing evidence that warrants another search. Do not search again when the requested facts already have sources.' } } } }
];
