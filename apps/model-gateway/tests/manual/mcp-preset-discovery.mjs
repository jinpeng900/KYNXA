// Explicit live publisher discovery only. This is not part of the offline Node test suite.
// No tools are called, no account environment is inherited, and every stdio profile/cache is temporary.
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { mcpPresetCatalog } from '../../mcp-presets.mjs';
import { McpToolClients } from '../../mcp-client.mjs';

const allowed = ['playwright', 'context7', 'chrome-devtools', 'exa', 'fetch', 'git', 'markitdown', 'dbhub'];
const selected = process.argv.slice(2);
if (!selected.length || selected.some(id => !allowed.includes(id))) throw Error('Select only public anonymous discovery presets: ' + allowed.join(', '));
const sharedUvCache = process.env.KYNXA_TEST_UV_CACHE_DIR;
const sharedNpmCache = process.env.KYNXA_TEST_NPM_CACHE_DIR;
for (const cache of [sharedUvCache, sharedNpmCache]) if (cache &&
  !relative(tmpdir(), resolve(cache)).split(/[\\/]/)[0].startsWith('kynxa-public-mcp-discovery-'))
  throw Error('Warm-cache discovery can reuse only a temporary public-discovery cache.');
const root = await mkdtemp(join(tmpdir(), 'kynxa-public-mcp-discovery-'));
const catalog = mcpPresetCatalog({ mcpServers: [] });
const report = { checkedAt: new Date().toISOString(), node: process.version, root, results: [] };
console.log(JSON.stringify({ temporaryRoot: root }));
for (const id of selected) {
  const preset = catalog.presets.find(item => item.id === id), workspace = join(root, id, 'workspace'), profile = join(root, id, 'profile');
  await mkdir(workspace, { recursive: true }); await mkdir(profile, { recursive: true });
  const npmUserConfig = join(profile, 'npmrc'); await writeFile(npmUserConfig, '');
  const server = { ...structuredClone(preset.server), enabled: true, startupTimeoutMs: 120000 };
  if (id === 'dbhub') {
    const configPath = join(workspace, 'dbhub-readonly.toml');
    await writeFile(configPath, preset.configurationTemplate);
    server.args = server.args.map(argument => argument === '<absolute-path-to-dbhub-readonly.toml>' ? configPath : argument);
    server.envRefs = {}; server.env = { ...server.env, DSN: 'sqlite:///:memory:' };
  }
  if (server.transport === 'stdio') server.env = { ...server.env, HOME: profile, USERPROFILE: profile,
    APPDATA: profile, LOCALAPPDATA: profile, XDG_CONFIG_HOME: profile, XDG_CACHE_HOME: profile, npm_config_userconfig: npmUserConfig,
    UV_CACHE_DIR: sharedUvCache ?? join(profile, 'uv-cache'), UV_PYTHON_INSTALL_DIR: join(profile, 'python'), UV_TOOL_DIR: join(profile, 'uv-tools'), UV_LINK_MODE: 'copy' };
  if (server.transport === 'stdio' && sharedNpmCache) server.env.npm_config_cache = sharedNpmCache;
  if (server.command === 'uvx') {
    if (process.env.KYNXA_TEST_UVX) server.command = process.env.KYNXA_TEST_UVX;
    if (process.env.KYNXA_TEST_PYTHON) server.args = ['--python', process.env.KYNXA_TEST_PYTHON, ...server.args];
  }
  const clients = new McpToolClients({ extensionRoot: join(root, id, 'extensions') });
  const started = Date.now();
  try {
    const tools = await clients.catalog({ mcpServers: [server] }, { workspaceRoot: workspace }, { connect: true, includeDisabled: true });
    const diagnostic = clients.diagnostics().find(item => item.serverId === id);
    const connection = (await Promise.all(clients.connections.values())).find(item => item.serverId === id);
    report.results.push({ id, package: preset.package ?? null, sourceUrl: preset.sourceUrl,
      success: diagnostic?.state === 'ready' && tools.length > 0, elapsedMs: Date.now() - started,
      protocolVersion: connection?.client.getNegotiatedProtocolVersion(), serverInfo: connection?.client.getServerVersion(),
      diagnostic, tools: tools.map(tool => ({ name: tool.toolName, operation: tool.operation })) });
    console.log(JSON.stringify(report.results.at(-1)));
  } catch (error) {
    report.results.push({ id, success: false, code: error.code ?? 'DISCOVERY_FAILED', elapsedMs: Date.now() - started });
    console.log(JSON.stringify(report.results.at(-1)));
  } finally { await clients.close(); }
}
const output = join(root, 'discovery-report.json'); await writeFile(output, JSON.stringify(report, null, 2));
console.log(JSON.stringify({ reportPath: output, passed: report.results.filter(item => item.success).length, total: report.results.length }));
if (report.results.some(item => !item.success)) process.exitCode = 1;
