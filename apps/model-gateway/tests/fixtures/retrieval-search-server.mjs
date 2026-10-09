import { appendFile } from 'node:fs/promises';
import { setTimeout as wait } from 'node:timers/promises';
import { McpServer } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { z } from 'zod';

// Synthetic SDK transport only; never searches the Internet or uses account credentials.
// 仅使用自造 SDK 传输，不查询互联网或使用账号凭据。
const log = process.argv[2];
serveStdio(() => {
  const server = new McpServer({ name: 'synthetic-retrieval', version: '1.0.0' });
  server.registerTool('web_search_exa', { description: 'Synthetic public multi-result search',
    inputSchema: z.object({ query: z.string(), numResults: z.number().int().optional() }).strict() }, async (input, context) => {
    await appendFile(log, JSON.stringify({ event: 'search', ...input }) + '\n');
    // Keep the reply pending until the client deadline cancels it, even under a busy test scheduler.
    // 繁忙测试调度下仍等客户端期限取消，不让正常回复抢在取消探针之前返回。
    if (input.query === 'slow-fixture') await wait(10000, undefined, { signal: context.signal });
    const value = { results: [{ url: 'https://example.com/announcement', title: 'Official announcement', text: 'Dated evidence for the query.' }] };
    return { content: [{ type: 'text', text: JSON.stringify(value) }], structuredContent: value };
  });
  server.registerTool('fetch', { description: 'Synthetic public reading', inputSchema: z.object({ url: z.string() }).strict() }, async input => {
    await appendFile(log, JSON.stringify({ event: 'fetch', ...input }) + '\n');
    return { content: [{ type: 'text', text: 'Synthetic public page.' }] };
  });
  return server;
});
