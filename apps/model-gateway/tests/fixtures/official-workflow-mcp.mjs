import { appendFile } from 'node:fs/promises';
import { setTimeout as delay } from 'node:timers/promises';
import { McpServer } from '@modelcontextprotocol/server';
import { serveStdio } from '@modelcontextprotocol/server/stdio';
import { z } from 'zod';

// Isolated test process only: these reserved-domain documents never contact the internet.
// 仅用于隔离测试；保留域名中的资料不访问互联网。
const [mode, log] = process.argv.slice(2);
const sources = [
  { title: 'Fixture release', url: 'https://sources.example.test/release', text: 'Version 2.1.0 was released on 2026-10-01.' },
  { title: 'Fixture changelog', url: 'https://sources.example.test/changelog', text: 'The 2.1.0 changelog confirms the October 1 release and a fixed parser.' }
];
await appendFile(log, JSON.stringify({ event: 'started', mode, pid: process.pid }) + '\n');
serveStdio(() => {
  const server = new McpServer({ name: 'isolated-workflow-' + mode, version: '1.0.0' });
  let active = 0;
  if (mode === 'search') server.registerTool('web_search_exa', {
    description: 'Search isolated release sources and return multiple direct URLs.',
    inputSchema: z.object({ query: z.string() }).strict()
  }, async input => {
    await appendFile(log, JSON.stringify({ event: 'search', query: input.query }) + '\n');
    return { content: [{ type: 'text', text: JSON.stringify({ results: sources.map(({ title, url }) => ({ title, url })) }) }],
      _meta: { privateMarker: 'PRIVATE_WORKFLOW_SEARCH_METADATA' } };
  });
  else server.registerTool('fetch', {
    description: 'Read one of the previously discovered isolated source URLs.',
    inputSchema: z.object({ url: z.string().url() }).strict()
  }, async input => {
    const source = sources.find(item => item.url === input.url);
    if (!source) throw new Error('Only fixture source URLs may be read.');
    active++;
    try {
      await appendFile(log, JSON.stringify({ event: 'fetch', url: input.url, active }) + '\n');
      await delay(100);
      return { content: [{ type: 'text', text: JSON.stringify(source) }], _meta: { privateMarker: 'PRIVATE_WORKFLOW_FETCH_METADATA' } };
    } finally {
      active--;
      await appendFile(log, JSON.stringify({ event: 'fetch_end', url: input.url, active }) + '\n');
    }
  });
  return server;
});
