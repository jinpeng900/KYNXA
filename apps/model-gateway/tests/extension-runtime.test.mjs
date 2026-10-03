import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { ModelRuntime } from '../runtime.mjs';
import { ModelStore } from '../store.mjs';
import { AppSkillService } from '../skill-service.mjs';
import { ToolService } from '../tool-service.mjs';
import { McpToolClients } from '../mcp-client.mjs';
import { extensionControlPaths } from '../extension-storage.mjs';
import { toolFixture, parsed } from './tool-fixture.mjs';

const skillContent = name => `---\nname: ${name}\ndescription: Synthetic package for isolated extension tests.\n---\nRead scripts/check.mjs.\n`;
async function saveSkill(directory, name) {
  const root = join(directory, name);
  await mkdir(join(root, 'scripts'), { recursive: true });
  await writeFile(join(root, 'SKILL.md'), skillContent(name));
  await writeFile(join(root, 'scripts', 'check.mjs'), 'console.log("synthetic");');
  return root;
}

test('independent extension config and skills leave conversations/results in formal Data and protect both private roots', async t => {
  const f = await toolFixture(t), extensionRoot = join(f.root, 'Extensions'), pointer = join(f.root, 'Settings', 'extensions.json');
  await mkdir(join(f.root, 'Settings')); await writeFile(pointer, JSON.stringify({ version: 1, extensionRoot }));
  const service = new ToolService({ conversationStore: f.conversations, dataHome: f.dataHome, extensionRoot,
    extensionPointer: pointer, bundledDirectory: null });
  t.after(() => service.close());
  const config = await service.updateConfig({ version: 1, expectedRevision: 0, mcpServers: [], skillDirectories: [], disabledSkills: [] });
  assert.equal(service.config.file, join(extensionRoot, 'Agent', 'config.json'));
  assert.equal((await f.service.getConfig()).revision, 0, 'formal Data did not receive extension configuration');
  await saveSkill(join(extensionRoot, 'Skills'), 'configured');
  const ctx = await service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full' });
  const run = (name, args) => service.execute(ctx, f.call(name, args));
  const skill = (await service.listSkills(ctx)).find(item => item.name === 'configured');
  assert.equal((await service.skills.readResource(skill.id, 'scripts/check.mjs', ctx, config)).content, 'console.log("synthetic");');
  for (const root of [extensionRoot, f.conversations.root]) {
    await mkdir(join(root, 'Backups'), { recursive: true });
    await writeFile(join(root, 'Backups', 'config.backup.json'), '{"private":"SYNTHETIC_CREDENTIAL"}');
    const path = join(await realpath(root), 'Backups', 'config.backup.json');
    assert.equal((await run('filesystem.read', { path, reason: 'Synthetic requested read' })).code, 'PROTECTED_MODEL_CREDENTIALS');
    const search = parsed(await run('filesystem.search', { path: join(root, 'Backups'), query: 'SYNTHETIC_CREDENTIAL', reason: 'Synthetic search' }));
    assert.deepEqual(search.matches, []); assert.ok(search.skipped > 0);
  }
  assert.equal((await run('filesystem.read', { path: service.config.file, reason: 'Synthetic read' })).code, 'PROTECTED_MODEL_CREDENTIALS');
  for (const path of [pointer, ...extensionControlPaths(pointer).filter(path => path.startsWith(join(f.root, 'Settings')))]) {
    for (const [name, args] of [['filesystem.write', { content: 'changed', expectedHash: null }],
      ['filesystem.edit', { oldText: 'version', newText: 'changed', expectedHash: 'a'.repeat(64) }],
      ['filesystem.delete', { expectedHash: 'a'.repeat(64) }], ['filesystem.mkdir', {}]])
      assert.equal((await run(name, { path, reason: 'Synthetic protected change', ...args })).code, 'PROTECTED_APP_DATA', `${name} ${path}`);
  }
  assert.equal((await run('filesystem.delete', { path: join(f.root, 'Settings'), expectedHash: 'a'.repeat(64), reason: 'Synthetic parent removal' })).code, 'PROTECTED_APP_DATA');
  assert.equal(JSON.parse(await readFile(pointer, 'utf8')).extensionRoot, extensionRoot);
  const output = await run('filesystem.list', { path: f.workspace });
  assert.ok(output.resultRef, 'full tool output remains stored by formal conversation service');
  const result = await service.results.get(ctx, output.resultRef.id);
  assert.equal(result.structuredContent.path, f.workspace);
  await f.conversations.withConversationStorage(f.conversationId, async relationship => {
    const document = JSON.parse(await readFile(join(relationship.sessionDirectory, 'tool-results', output.resultRef.id + '.json'), 'utf8'));
    assert.equal(document.conversationId, f.conversationId);
    assert.ok(relationship.sessionDirectory.startsWith(f.conversations.root));
  });
  const runtime = new ModelRuntime({ modelStore: new ModelStore({ dataHome: f.dataHome }), dataHome: f.dataHome,
    conversationStore: f.conversations, toolService: service, extensionRoot });
  assert.equal(runtime.extensionRoot, extensionRoot); assert.equal(runtime.conversations.root, f.conversations.root);
});

test('skills in an extension root below Data are public while formal records remain denied through ancestor packages', async t => {
  const f = await toolFixture(t), formal = f.conversations.root, extensionRoot = join(formal, 'Extensions');
  const service = new AppSkillService(extensionRoot, { ownedDataRoot: formal, bundledDirectory: null });
  await saveSkill(join(extensionRoot, 'Skills'), 'nested');
  await writeFile(join(f.root, 'SKILL.md'), skillContent('ancestor'));
  await mkdir(join(formal, 'Agent'), { recursive: true });
  await writeFile(join(formal, 'Agent', 'config.json'), '{"private":"SYNTHETIC_CREDENTIAL"}');
  const privateResult = join(formal, 'Chats', randomUUID(), 'tool-results', randomUUID() + '.json');
  await mkdir(join(privateResult, '..'), { recursive: true }); await writeFile(privateResult, 'SYNTHETIC_RAW_RESULT');
  const config = { skillDirectories: [f.root], disabledSkills: [] };
  const list = await service.list(undefined, config), nested = list.find(skill => skill.name === 'nested'), ancestor = list.find(skill => skill.name === 'ancestor');
  assert.ok(nested); assert.ok(ancestor);
  assert.equal((await service.readResource(nested.id, 'scripts/check.mjs', undefined, config)).content, 'console.log("synthetic");');
  for (const target of [join(formal, 'Agent', 'config.json'), privateResult])
    await assert.rejects(service.readResource(ancestor.id, relative(f.root, target), undefined, config), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inspected = await service.inspect(ancestor.id, undefined, config);
  assert.equal(inspected.packageValid, false); assert.ok(inspected.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
  await assert.rejects(service.prepareScript(ancestor.id, 'scripts/check.mjs', undefined, config), { code: 'UNSAFE_SKILL_PACKAGE' });
  await assert.rejects(service.importPackage(f.root), error => error.code === 'INVALID_APP_SKILL_IMPORT' &&
    error.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
});

test('a formal Data tree beneath Extensions/Skills is never mistaken for public skill resources', async t => {
  const f = await toolFixture(t), extensionRoot = join(f.root, 'NestedExtensions'), publicRoot = join(extensionRoot, 'Skills');
  const formal = join(publicRoot, 'FormalData'), service = new AppSkillService(extensionRoot, { ownedDataRoot: formal, bundledDirectory: null });
  await mkdir(join(publicRoot, 'scripts'), { recursive: true });
  await writeFile(join(publicRoot, 'SKILL.md'), skillContent('skills'));
  await writeFile(join(publicRoot, 'scripts', 'check.mjs'), 'console.log("public");');
  await mkdir(join(formal, 'Agent'), { recursive: true });
  await writeFile(join(formal, 'Agent', 'config.json'), '{"private":"SYNTHETIC_FORMAL_SECRET"}');
  const raw = join(formal, 'Chats', randomUUID(), 'tool-results', randomUUID() + '.json');
  await mkdir(join(raw, '..'), { recursive: true }); await writeFile(raw, 'SYNTHETIC_RAW_RESULT');
  const config = { skillDirectories: [], disabledSkills: [] }, skill = (await service.list(undefined, config)).find(item => item.name === 'skills');
  assert.ok(skill);
  assert.equal((await service.readResource(skill.id, 'scripts/check.mjs', undefined, config)).content, 'console.log("public");');
  for (const path of [join(formal, 'Agent', 'config.json'), raw])
    await assert.rejects(service.readResource(skill.id, relative(publicRoot, path), undefined, config), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inventory = await service.inspect(skill.id, undefined, config);
  assert.equal(inventory.packageValid, false); assert.ok(inventory.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
  await assert.rejects(service.prepareScript(skill.id, 'scripts/check.mjs', undefined, config), { code: 'UNSAFE_SKILL_PACKAGE' });
  await assert.rejects(service.importPackage(publicRoot), error => error.code === 'INVALID_APP_SKILL_IMPORT' &&
    error.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
});

test('managed work permissions cannot mutate colocated extension metadata or use its root as a terminal snapshot', async t => {
  const f = await toolFixture(t), managed = join(f.conversations.root, 'Desktop', 'Projects', f.projectId);
  await mkdir(managed, { recursive: true });
  const catalog = await f.conversations.catalog(); catalog.Projects[0].FolderPath = managed; await f.conversations.saveCatalog(catalog);
  const service = new ToolService({ conversationStore: f.conversations, dataHome: f.dataHome, extensionRoot: managed, bundledDirectory: null });
  t.after(() => service.close());
  await service.updateConfig({ version: 1, expectedRevision: 0, mcpServers: [], skillDirectories: [], disabledSkills: [] });
  const ctx = await service.createContext(f.conversationId, { requestId: randomUUID(), permissionMode: 'full' });
  assert.equal(ctx.managedWorkspace, true);
  for (const path of ['Agent/config.json', 'Skills/package/SKILL.md', 'MCP/npm-cache/entry.json', 'Backups/config.json', 'extension-migration-info.json']) {
    await mkdir(join(managed, path, '..'), { recursive: true });
    const file = join(await realpath(managed), path);
    for (const [name, args] of [['filesystem.write', { content: 'changed', expectedHash: null }],
      ['filesystem.edit', { oldText: 'old', newText: 'new', expectedHash: 'a'.repeat(64) }],
      ['filesystem.delete', { expectedHash: 'a'.repeat(64) }], ['filesystem.mkdir', {}]])
      assert.equal((await service.execute(ctx, f.call(name, { path: file, reason: 'Synthetic canonical alias protection', ...args }))).code, 'PROTECTED_APP_DATA', `${name} ${path}`);
  }
  assert.equal((await service.execute(ctx, f.call('terminal.run', { command: 'node', args: [] }))).code, 'PROTECTED_APP_DATA');
  parsed(await service.execute(ctx, f.call('filesystem.write', { path: 'ordinary-work.txt', content: 'public work', expectedHash: null })));
  assert.equal(await readFile(join(managed, 'ordinary-work.txt'), 'utf8'), 'public work');
});

test('real SDK stdio receives independent default caches and explicit env/envRefs keep precedence', { timeout: 20000 }, async t => {
  const f = await toolFixture(t), extensionRoot = join(f.root, 'Extensions');
  const script = join(f.root, 'capture-cache.mjs'), log = join(f.root, 'events.jsonl'), capture = join(f.root, 'environment.json');
  await writeFile(script, `import {writeFile} from 'node:fs/promises';
await writeFile(${JSON.stringify(capture)},JSON.stringify({npm:process.env.npm_config_cache,browser:process.env.PLAYWRIGHT_BROWSERS_PATH}));
await import(${JSON.stringify(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url).href)});`);
  const clients = new McpToolClients({ extensionRoot }); t.after(() => clients.close());
  const server = { id: 'cache-test', name: 'Synthetic cache test', command: process.execPath, args: [script, log], enabled: true, cwd: f.root };
  let tools = await clients.catalog({ mcpServers: [server] }, { workspaceRoot: f.workspace }, { connect: true });
  assert.ok(tools.some(tool => tool.toolName === 'echo'), JSON.stringify(clients.diagnostics()));
  assert.deepEqual(JSON.parse(await readFile(capture, 'utf8')), { npm: join(extensionRoot, 'MCP', 'npm-cache'), browser: join(extensionRoot, 'MCP', 'browser-cache') });
  await clients.reset();
  const variable = 'KYNXA_TEST_EXTENSION_BROWSER_' + randomUUID().replaceAll('-', '');
  process.env[variable] = join(f.root, 'explicit-browser');
  t.after(() => { delete process.env[variable]; });
  tools = await clients.catalog({ mcpServers: [{ ...server, env: { npm_config_cache: join(f.root, 'explicit-npm') },
    envRefs: { PLAYWRIGHT_BROWSERS_PATH: variable } }] }, { workspaceRoot: f.workspace }, { connect: true });
  const echo = tools.find(tool => tool.toolName === 'echo'); assert.ok(echo);
  assert.equal((await clients.execute(echo, { arguments: { value: 'cache-ready' }, policy: { reason: 'Synthetic cache validation' } })).content, 'echo:cache-ready');
  assert.deepEqual(JSON.parse(await readFile(capture, 'utf8')), { npm: join(f.root, 'explicit-npm'), browser: join(f.root, 'explicit-browser') });
  await clients.close();
});

test('real SDK teardown failures remain owned and block reset/disconnect/reconnect without leaking details or replaying cleanup', { timeout: 20000 }, async t => {
  for (const method of ['reset', 'disconnect']) await t.test(method, async childTest => {
    const f = await toolFixture(childTest), clients = new McpToolClients();
    const log = join(f.root, 'cleanup-events.jsonl');
    const server = { id: 'failed-close', name: 'Synthetic teardown test', command: process.execPath,
      args: [fileURLToPath(new URL('./fixtures/mcp-tool-server.mjs', import.meta.url)), log], enabled: true };
    const healthy = { ...server, id: 'healthy-close', args: [...server.args] }, config = { mcpServers: [server, healthy] };
    const tools = await clients.catalog(config, {}, { connect: true }); assert.ok(tools.length > 0);
    const entries = await Promise.all([...clients.connections].map(async ([key, operation]) => [key, await operation]));
    const [key, failed] = entries.find(([, connection]) => connection.serverId === server.id);
    const original = new Map(entries.map(([, connection]) => [connection, connection.client.close.bind(connection.client)]));
    let closeAttempts = 0;
    failed.client.close = async () => { closeAttempts++; throw Error('PRIVATE_CLEANUP_DETAILS_MUST_NOT_LEAK'); };
    try {
      await assert.rejects(method === 'reset' ? clients.reset() : clients.disconnect(server.id), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
      assert.equal(clients.failedClosures.get(key), failed, 'failed client/transport stays owned until process restart');
      assert.equal(clients.diagnostics().find(state => state.serverId === server.id).code, 'MCP_PROCESS_CLEANUP_FAILED');
      assert.equal(JSON.stringify(clients.diagnostics()).includes('PRIVATE_CLEANUP_DETAILS'), false);
      await assert.rejects(clients.reconnect(server, {}), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
      assert.deepEqual(await clients.catalog(config, {}, { connect: true }), []);
      await assert.rejects(clients.execute(tools.find(tool => tool.serverId === server.id),
        { arguments: { value: 'never-replayed' }, policy: { reason: 'Synthetic test' } }), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
      await assert.rejects(clients.close(), { code: 'MCP_PROCESS_CLEANUP_FAILED' });
      assert.equal(closeAttempts, 1, 'a rejected owned close is not retried or hidden by SDK state resets');
      const events = (await readFile(log, 'utf8')).trim().split('\n').map(JSON.parse);
      assert.equal(events.filter(event => event.event === 'started').length, 2);
      assert.equal(events.some(event => event.value === 'never-replayed'), false);
    } finally {
      // Test-only cleanup uses the exact captured SDK owners, never names or broad process searches.
      await Promise.all([...original.values()].map(close => close()));
    }
  });
});

test('managed gateway serializes pointer transitions, waits for active requests and refuses damaged pointers before closing', { timeout: 20000 }, async t => {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-extension-runtime-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(home)); assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  const source = `
    import assert from 'node:assert/strict';
    import {mkdir,rm,writeFile} from 'node:fs/promises';
    import {join} from 'node:path';
    import {randomUUID} from 'node:crypto';
    const [home,serverModule,runtimeModule] = process.argv.slice(1);
    const profile=join(home,'.kynxa'),data=join(home,'Data'),first=join(home,'Extensions1'),second=join(home,'Extensions2');
    await mkdir(profile); await mkdir(join(data,'Models'),{recursive:true});
    const pointer=join(profile,'extensions.json'),lock=join(profile,'storage-migration.lock');
    await writeFile(pointer,JSON.stringify({version:1,extensionRoot:first}));
    process.env.KYNXA_DATA_HOME=data;process.env.KYNXA_EXTENSION_POINTER=pointer;
    const {ModelRuntime}=await import(runtimeModule),originalClose=ModelRuntime.prototype.close;
    let closes=0,closing=false,replyStarted,finishReply;
    const started=new Promise(ready=>{replyStarted=ready;}),replyGate=new Promise(ready=>{finishReply=ready;});
    ModelRuntime.prototype.close=async function(){assert.equal(closing,false);closing=true;closes++;await new Promise(ready=>setTimeout(ready,50));await originalClose.call(this);closing=false;};
    ModelRuntime.prototype.reply=async function(){replyStarted();await replyGate;return 'synthetic reply';};
    const {createModelServer}=await import(serverModule),server=createModelServer();
    await new Promise(ready=>server.listen(0,'127.0.0.1',ready));
    const base='http://127.0.0.1:'+server.address().port;
    const health=()=>fetch(base+'/health').then(response=>response.json());
    try{
      assert.equal((await health()).extensionRoot,first);
      const request=fetch(base+'/api/chat',{method:'POST',body:JSON.stringify({conversationId:randomUUID(),message:'synthetic'})});
      await started;
      await writeFile(pointer,JSON.stringify({version:1,extensionRoot:second}));
      let current=await health();assert.equal(current.activeRequests,1);assert.equal(current.extensionRoot,first);assert.equal(closes,0);
      await writeFile(lock,JSON.stringify({pid:process.pid}));finishReply();assert.equal((await request).status,200);
      assert.equal((await fetch(base+'/api/agent/config')).status,503);assert.equal((await health()).extensionRoot,first);
      await rm(lock);
      const views=await Promise.all(Array.from({length:12},()=>health()));
      assert.ok(views.every(view=>view.extensionRoot===second));assert.equal(closes,1);
      assert.ok(views.every(view=>view.agentProtocol===5&&view.extensionStorageProtocol===1&&view.modelDataHome===join(data,'Models')));
      await writeFile(pointer,'{"version":1,"version":1}');
      current=await health();assert.equal(current.extensionRoot,second);assert.equal(current.storageConfigError,'INVALID_EXTENSION_STORAGE');assert.equal(closes,1);
      assert.equal((await fetch(base+'/api/agent/config')).status,503);
      await writeFile(pointer,JSON.stringify({version:1,extensionRoot:second}));
      assert.equal((await health()).storageConfigError,undefined);assert.equal(closes,1);
      const responses=await Promise.all(Array.from({length:8},()=>fetch(base+'/api/agent/config')));
      assert.ok(responses.every(response=>response.status===200));
      process.stdout.write(JSON.stringify({closes,root:current.extensionRoot}));
    }finally{server.closeAllConnections();await new Promise(ready=>server.close(ready));await server.shutdownModelRuntime();}
  `;
  const child = spawn(process.execPath, ['--no-warnings', '--unhandled-rejections=strict', '--input-type=module', '-e', source,
    home, new URL('../server.mjs', import.meta.url).href, new URL('../runtime.mjs', import.meta.url).href], {
    env: { ...process.env, USERPROFILE: home, HOME: home, APPDATA: join(home, 'AppData'), LOCALAPPDATA: join(home, 'LocalAppData'),
      KYNXA_MODEL_HOME: '', KYNXA_EXTENSION_HOME: '', KYNXA_EXTENSION_POINTER: '', KYNXA_LEGACY_DESKTOP_HOME: join(home, 'LegacyDesktop') },
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe']
  });
  let stdout = '', stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
  child.stdout.on('data', value => { stdout += value; }); child.stderr.on('data', value => { stderr += value; });
  const completed = new Promise((ready, reject) => { child.once('error', reject); child.once('close', (code, signal) => ready({ code, signal })); });
  const timer = setTimeout(() => child.kill(), 15000);
  t.after(async () => { clearTimeout(timer); if (child.exitCode === null && child.signalCode === null) child.kill(); await completed; });
  const result = await completed; clearTimeout(timer);
  assert.equal(result.code, 0, stderr); assert.equal(result.signal, null);
  assert.deepEqual(JSON.parse(stdout), { closes: 1, root: join(home, 'Extensions2') });
});
