import { appendFile } from 'node:fs/promises';
import { McpServer, inputRequired } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { z } from 'zod';

// A synthetic official-SDK process. All paths and values are supplied by isolated tests.
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
  return server;
});
