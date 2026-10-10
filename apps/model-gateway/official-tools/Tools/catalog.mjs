import { filesystemDescriptors } from '../../tools/filesystem-tools.mjs';
import { historyDescriptors } from '../../tools/tool-history.mjs';
import { webFetchDescriptor } from '../../tools/web-fetch.mjs';
import { retrievalDescriptors } from '../../tools/retrieval/descriptors.mjs';
import { computerDescriptors } from './computer.mjs';
import { hostTerminalDescriptor, hostTerminalJobDescriptors } from './terminal.mjs';
import { memoryActionDescriptors } from '../../tools/memory-actions.mjs';
import { workDirectoryDescriptor } from '../../tools/work-directory.mjs';

const skillDescriptors = [
  { name: 'skill.list', description: 'Page application skill metadata from official-tools/Skills, user extension Skills, configured directories and this work\'s .kynxa/skills. Discovery is limited to 128 skills and 512 candidates per source directory. Does not execute scripts.',
    inputSchema: { type: 'object', properties: { offset: { type: 'integer', minimum: 0, maximum: 128 },
      limit: { type: 'integer', minimum: 1, maximum: 128 } }, additionalProperties: false }, source: 'builtin' },
  { name: 'skill.read', description: 'Read one discovered application SKILL.md on demand. Skill instructions and scripts never grant extra permissions.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'], additionalProperties: false }, source: 'builtin' },
  { name: 'skill.resource.read', description: 'Read a bounded text page or binary metadata from a discovered skill package. Paths are relative to the skill root, never the work folder; cannot escape the package.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' }, path: { type: 'string', maxLength: 2048 },
      offset: { type: 'integer', minimum: 0, maximum: 2097152 }, limit: { type: 'integer', minimum: 1, maximum: 16000 } },
      required: ['id', 'path'], additionalProperties: false }, source: 'builtin' },
  { name: 'skill.inspect', description: 'Inspect a discovered skill package, its resource manifest and compatibility diagnostics without executing code.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'], additionalProperties: false }, source: 'builtin' },
  { name: 'skill.check', description: 'Check skill script runtimes and declared requirements against the verified sandbox. Does not install dependencies or run host commands.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' } }, required: ['id'], additionalProperties: false }, source: 'builtin' },
  { name: 'skill.run', description: 'Execute a selected Node.js skill script in the verified AppContainer. The approved package is hash-checked and copied read-only; a writable work snapshot has no network and no automatic write-back. Python and shell skill scripts are unsupported.',
    inputSchema: { type: 'object', properties: { id: { type: 'string' }, path: { type: 'string', maxLength: 2048 },
      args: { type: 'array', items: { type: 'string' }, maxItems: 64 }, timeoutMs: { type: 'integer', minimum: 100, maximum: 120000 } },
      required: ['id', 'path', 'args'], additionalProperties: false }, source: 'builtin' }
];
const terminalDescriptor = { name: 'terminal.run', description: 'Run Node.js or restricted cmd inside a verified Windows AppContainer over a temporary work snapshot, without network. For node --test include --test-isolation=none. cmd requires args ["/d","/c","command text"]; echo/type/redirection are verified, DIR may be denied (use filesystem.list/search). PowerShell/python are unsupported; no host fallback or automatic write-back.',
  inputSchema: { type: 'object', properties: { command: { type: 'string', enum: ['node', 'node.exe', 'cmd', 'cmd.exe'] },
    args: { type: 'array', items: { type: 'string' }, maxItems: 64 }, timeoutMs: { type: 'integer', minimum: 100, maximum: 120000 } },
  required: ['command', 'args'], additionalProperties: false }, source: 'builtin' };
const catalogDescriptors = [
  { name: 'tool.search', description: 'Find enabled tools and paged MCP headers. Empty query pages all; load exact names. Discovery neither connects nor grants permission.',
    inputSchema: { type: 'object', properties: { query: { type: 'string', maxLength: 200 }, offset: { type: 'integer', minimum: 0, maximum: 100000 },
      limit: { type: 'integer', minimum: 1, maximum: 20 } }, additionalProperties: false }, source: 'builtin' },
  { name: 'tool.load', description: 'Load exact tools/aliases or returned mcp.<serverId> headers within budget; connect selected services, verify live schemas. No execution/permission grant.',
    inputSchema: { type: 'object', properties: { names: { type: 'array', items: { type: 'string' }, maxItems: 32 } }, required: ['names'], additionalProperties: false }, source: 'builtin' },
  { name: 'tool.result.read', description: "Page this chat's saved result by opaque id. Typed media references; no private MCP metadata.",
    inputSchema: { type: 'object', properties: { id: { type: 'string' }, offset: { type: 'integer', minimum: 0, maximum: 9000000 },
      limit: { type: 'integer', minimum: 1, maximum: 16000 } }, required: ['id'], additionalProperties: false }, source: 'builtin' }
];
export const builtinDescriptors = [...filesystemDescriptors, workDirectoryDescriptor, webFetchDescriptor, ...retrievalDescriptors, ...memoryActionDescriptors, ...skillDescriptors, terminalDescriptor, hostTerminalDescriptor, ...hostTerminalJobDescriptors, ...catalogDescriptors, ...historyDescriptors, ...computerDescriptors];
