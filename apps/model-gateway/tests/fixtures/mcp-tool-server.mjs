import { appendFile } from 'node:fs/promises';
import { McpServer, inputRequired } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { z } from 'zod';

// A synthetic official-SDK process. All paths and values are supplied by isolated tests.
// 使用官方 SDK 的自造测试进程；所有路径和值均由隔离测试提供。
const log = process.argv[2];
await appendFile(log, JSON.stringify({ event: 'started', pid: process.pid }) + '\n');
serveStdio(() => {
  const server = new McpServer({ name: 'synthetic-tool-server', version: '1.0.0' });
  server.registerTool('echo', { description: 'Synthetic echo with intentionally untrusted read-only annotations',
    inputSchema: z.object({ value: z.string() }).strict(), annotations: { readOnlyHint: true, destructiveHint: false } }, async ({ value }) => {
    await appendFile(log, JSON.stringify({ event: 'echo', value }) + '\n');
    return { content: [{ type: 'text', text: `echo:${value}` }] };
  });
  server.registerTool('slow', { description: 'Synthetic cancellable tool', inputSchema: z.object({}).strict() }, async (_args, context) => {
    await appendFile(log, JSON.stringify({ event: 'slow' }) + '\n');
    await new Promise(resolve => { const timer = setTimeout(resolve, 30000); context.signal?.addEventListener('abort', () => { clearTimeout(timer); resolve(); }, { once: true }); });
    return { content: [{ type: 'text', text: 'slow-completed' }] };
  });
  server.registerTool('needs_input', { description: 'Synthetic modern multi-round-trip result', inputSchema: z.object({}).strict() }, async () => {
    await appendFile(log, JSON.stringify({ event: 'needs_input' }) + '\n');
    return inputRequired({ requestState: 'synthetic-confirmation-token' });
  });
  server.registerTool('numeric_reason', { description: 'Business reason is a required integer, unrelated to application policy',
    inputSchema: z.object({ reason: z.number().int(), value: z.string() }).strict() }, async args => {
    await appendFile(log, JSON.stringify({ event: 'numeric_reason', arguments: args }) + '\n');
    return { content: [{ type: 'text', text: JSON.stringify(args) }] };
  });
  server.registerTool('optional_reason', { description: 'Business reason is optional and must remain optional',
    inputSchema: z.object({ value: z.string(), reason: z.number().int().optional() }).strict() }, async args => {
    await appendFile(log, JSON.stringify({ event: 'optional_reason', arguments: args }) + '\n');
    return { content: [{ type: 'text', text: JSON.stringify(args) }] };
  });
  server.registerTool('shadow_fields', { description: 'Business fields may share names with the application envelope',
    inputSchema: z.object({ arguments: z.string(), policy: z.string(), reason: z.number().int() }).strict() }, async args => {
    await appendFile(log, JSON.stringify({ event: 'shadow_fields', arguments: args }) + '\n');
    return { content: [{ type: 'text', text: JSON.stringify(args) }] };
  });
  const referenceSchema = { type: 'object', properties: { value: { $ref: '#/$defs/label' },
    literal: { type: 'object', default: { $ref: '#/$defs/literal-data' } },
    ownResource: { $id: 'urn:synthetic:mcp:nested', type: 'object', properties: { local: { $ref: '#/$defs/local' } },
      $defs: { local: { type: 'string' } } } }, $defs: { label: { type: 'string', minLength: 1 } },
    required: ['value'], additionalProperties: false };
  const referenceValidator = z.object({ value: z.string().min(1) }).strict();
  const standardReferenceSchema = { '~standard': { version: 1, vendor: 'synthetic-reference-schema',
    validate: value => referenceValidator['~standard'].validate(value),
    jsonSchema: { input: () => structuredClone(referenceSchema), output: () => structuredClone(referenceSchema) } } };
  server.registerTool('reference_schema', { description: 'Business schema with root-local refs and literal ref-shaped metadata',
    inputSchema: standardReferenceSchema }, async args => ({ content: [{ type: 'text', text: JSON.stringify(args) }] }));
  const draft7TupleSchema = { $schema: 'http://json-schema.org/draft-07/schema#', type: 'object',
    properties: { row: { type: 'array', items: [{ type: 'string' }, { type: 'integer' }], additionalItems: false,
      minItems: 2, maxItems: 2 } }, required: ['row'], additionalProperties: false };
  const tupleValidator = z.object({ row: z.tuple([z.string(), z.number().int()]) }).strict();
  server.registerTool('draft7_tuple', { description: 'Synthetic legacy JSON Schema tuple dialect',
    inputSchema: { '~standard': { version: 1, vendor: 'synthetic-draft7-schema',
      validate: value => tupleValidator['~standard'].validate(value),
      jsonSchema: { input: () => structuredClone(draft7TupleSchema), output: () => structuredClone(draft7TupleSchema) } } } }, async args => {
    await appendFile(log, JSON.stringify({ event: 'draft7_tuple', arguments: args }) + '\n');
    return { content: [{ type: 'text', text: JSON.stringify(args) }] };
  });
  server.registerTool('typed_result', { description: 'Synthetic typed MCP result with complete client-only metadata',
    inputSchema: z.object({}).strict() }, async () => {
    await appendFile(log, JSON.stringify({ event: 'typed_result' }) + '\n');
    return { content: [
      { type: 'text', text: 'Synthetic human-readable summary.', _meta: { clientOnly: 'private-text-metadata' } },
      { type: 'image', mimeType: 'image/png', data: 'c3ludGhldGljLWltYWdlLWJ5dGVz', _meta: { clientOnly: 'private-image-metadata' } },
      { type: 'audio', mimeType: 'audio/wav', data: 'c3ludGhldGljLWF1ZGlvLWJ5dGVz' },
      { type: 'resource_link', uri: 'memory://synthetic/report.json', name: 'Synthetic report', mimeType: 'application/json' },
      { type: 'resource', resource: { uri: 'memory://synthetic/note.txt', mimeType: 'text/plain', text: 'Embedded synthetic resource text.',
        _meta: { clientOnly: 'private-resource-metadata' } } },
      { type: 'resource', resource: { uri: 'memory://synthetic/binary', mimeType: 'application/octet-stream', blob: 'c3ludGhldGljLWJpbmFyeQ==' } }
    ], structuredContent: { report: { count: 2, value: 'Structured result survives with text.', _meta: { clientOnly: 'private-structured-metadata' } } },
    _meta: { clientOnly: 'private-top-level-metadata' }, isError: false };
  });
  server.registerTool('navigate_page', { description: 'Synthetic Chrome-style navigation result',
    inputSchema: z.object({ failed: z.boolean() }).strict() }, async args => ({
    content: [{ type: 'text', text: args.failed ? 'Unable to navigate in the selected page: net::ERR_CONNECTION_REFUSED.\n## Pages\n1: old page'
      : 'Successfully navigated to https://example.com/.\n## Pages\n1: Example Domain' }], isError: false
  }));
  server.registerTool('long_result', { description: 'Synthetic large structured result', inputSchema: z.object({}).strict() }, async () => ({
    content: [{ type: 'text', text: 'Large synthetic structured result.' }],
    structuredContent: { data: 'synthetic-long-output-'.repeat(6000) }
  }));
  if (process.argv[3] === 'many') for (let index = 0; index < 100; index++) {
    const name = `large_${String(index).padStart(3, '0')}`;
    server.registerTool(name, { description: `Synthetic deferred directory tool ${name}`, inputSchema: z.object({ value: z.string() }).strict() }, async args => {
      await appendFile(log, JSON.stringify({ event: name, arguments: args }) + '\n');
      return { content: [{ type: 'text', text: `${name}:${args.value}` }] };
    });
  }
  return server;
});
