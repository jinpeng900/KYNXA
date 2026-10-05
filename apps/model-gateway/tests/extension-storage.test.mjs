import assert from 'node:assert/strict';
import { existsSync, readdirSync, writeFileSync } from 'node:fs';
import { access, link, mkdir, mkdtemp, readFile, readdir, rm, stat, symlink, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, parse, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ensureExtensionLayout, inspectExtensionLayout, EXTENSION_LAYOUT_DIRECTORIES, extensionCacheDirectories, extensionControlPaths,
  extensionHome, extensionPointerPath, isExtensionControlPath, isExtensionManagedPath } from '../data/extension-storage.mjs';

async function temporary(t) {
  const home = await mkdtemp(join(tmpdir(), 'kynxa-extension-path-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), home);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(home, { recursive: true, force: true });
  });
  await mkdir(join(home, '.kynxa'));
  return home;
}

test('extension location has portable pointer precedence and isolated Data/Model overrides preserve legacy defaults', async t => {
  const home = await temporary(t), dataHome = join(home, 'Data', 'Models'), extensionRoot = join(home, 'Extensions');
  const pointer = join(home, '.kynxa', 'extensions.json');
  assert.equal(extensionHome(dataHome, {}, home), join(home, 'Data'));
  assert.equal(extensionHome(join(home, 'legacy-models'), {}, home), join(home, 'legacy-models', 'Conversations'));
  await writeFile(pointer, '\uFEFF' + JSON.stringify({ version: 1, extensionRoot }));
  assert.equal(extensionHome(dataHome, {}, home), extensionRoot);
  assert.equal(extensionHome(dataHome, { KYNXA_DATA_HOME: join(home, 'Data') }, home), join(home, 'Data'));
  assert.equal(extensionHome(dataHome, { KYNXA_MODEL_HOME: dataHome }, home), join(home, 'Data'));
  assert.equal(extensionHome(dataHome, { KYNXA_DATA_HOME: join(home, 'Data'), KYNXA_EXTENSION_POINTER: pointer }, home), extensionRoot);
  await writeFile(pointer, 'broken');
  assert.equal(extensionHome(dataHome, { KYNXA_EXTENSION_HOME: extensionRoot }, home), extensionRoot);
  assert.equal(extensionHome(dataHome, { KYNXA_DATA_HOME: join(home, 'Data') }, home), join(home, 'Data'));
  assert.deepEqual(extensionCacheDirectories(extensionRoot), { npmCache: join(extensionRoot, 'MCP', 'npm-cache'),
    browserCache: join(extensionRoot, 'MCP', 'browser-cache'), uvCache: join(extensionRoot, 'MCP', 'uv-cache'),
    uvTools: join(extensionRoot, 'MCP', 'uv-tools'), python: join(extensionRoot, 'MCP', 'python'),
    pythonBin: join(extensionRoot, 'MCP', 'python-bin'), uvToolBin: join(extensionRoot, 'MCP', 'bin') });
});

test('pointer validation matches native local paths, 16KiB, strict UTF8 and duplicate-field semantics without changing originals', async t => {
  const home = await temporary(t), pointer = join(home, '.kynxa', 'extensions.json'), dataHome = join(home, 'Data', 'Models');
  const extensionRoot = join(home, 'Extensions');
  const cases = [
    [JSON.stringify({ version: 2, extensionRoot }), 'UNSUPPORTED_EXTENSION_STORAGE'],
    [JSON.stringify({ version: 1, extensionRoot: 'relative' }), 'INVALID_EXTENSION_STORAGE'],
    [JSON.stringify({ version: 1, extensionRoot: parse(home).root }), 'INVALID_EXTENSION_STORAGE'],
    [JSON.stringify({ version: 1, extensionRoot: '\\\\server\\share\\extensions' }), 'INVALID_EXTENSION_STORAGE'],
    ['{"version":0,"version":1,"extensionRoot":' + JSON.stringify(extensionRoot) + '}', 'INVALID_EXTENSION_STORAGE'],
    ['{"version":1,"extensionRoot":' + JSON.stringify(extensionRoot) + ',"extension\\u0052oot":' + JSON.stringify(extensionRoot) + '}', 'INVALID_EXTENSION_STORAGE'],
    [' '.repeat(16385), 'INVALID_EXTENSION_STORAGE'],
    [Buffer.from([0x7b, 0xff, 0x7d]), 'INVALID_EXTENSION_STORAGE']
  ];
  for (const [value, code] of cases) {
    await writeFile(pointer, value);
    const original = await readFile(pointer);
    assert.throws(() => extensionHome(dataHome, { KYNXA_EXTENSION_POINTER: pointer }, home), { code });
    assert.deepEqual(await readFile(pointer), original);
  }
  const custom = join(home, 'configuration', 'extension-location.json');
  assert.equal(extensionPointerPath({ KYNXA_EXTENSION_POINTER: custom }, home), custom);
  assert.equal(extensionPointerPath({}, home), pointer);
  for (const path of [...extensionControlPaths(custom, home), join(home, 'configuration'), join(home, '.kynxa')])
    assert.equal(isExtensionControlPath(path, custom, home), true);
  assert.equal(isExtensionControlPath(join(home, 'configuration', 'notes.txt'), custom, home), false);
  assert.equal(isExtensionControlPath(join(home, 'ordinary'), custom, home), false);
});

test('pointer and selected root reject linked ancestor directories', async t => {
  const home = await temporary(t), real = join(home, 'real'), link = join(home, 'linked'), dataHome = join(home, 'Data', 'Models');
  await mkdir(real); await writeFile(join(real, 'extensions.json'), JSON.stringify({ version: 1, extensionRoot: join(home, 'Extensions') }));
  try { await symlink(real, link, process.platform === 'win32' ? 'junction' : 'dir'); }
  catch (error) { if (['EPERM', 'EACCES'].includes(error.code)) return t.skip('This account cannot create a temporary directory link.'); throw error; }
  assert.throws(() => extensionHome(dataHome, { KYNXA_EXTENSION_POINTER: join(link, 'extensions.json') }, home), { code: 'INVALID_EXTENSION_STORAGE' });
  assert.throws(() => extensionHome(dataHome, { KYNXA_EXTENSION_HOME: join(link, 'Extensions') }, home), { code: 'INVALID_EXTENSION_STORAGE' });
});

test('first extension activation builds a complete framework and preserves user config and metadata on subsequent startup', async t => {
  const home = await temporary(t), root = join(home, 'Extensions');
  assert.equal(inspectExtensionLayout(root), null);
  await assert.rejects(access(root), { code: 'ENOENT' }, 'inspection does not initialize');
  await Promise.all([ensureExtensionLayout(root), ensureExtensionLayout(root)]);
  for (const name of EXTENSION_LAYOUT_DIRECTORIES) assert.equal((await stat(join(root, name))).isDirectory(), true, name);
  const metadata = join(root, 'extension-layout.json');
  await writeFile(metadata, JSON.stringify({ version: 1, futureMetadata: 'preserved' }));
  const config = join(root, 'Agent', 'config.json'), content = '{"version":1,"revision":7,"mcpServers":[],"skillDirectories":[],"disabledSkills":[]}';
  await writeFile(config, content);
  const before = await stat(metadata);
  await ensureExtensionLayout(root);
  assert.deepEqual(inspectExtensionLayout(root), { version: 1, futureMetadata: 'preserved' });
  assert.equal((await stat(metadata)).mtimeMs, before.mtimeMs);
  assert.equal(await readFile(config, 'utf8'), content);
  for (const name of ['extension-layout.json', 'extensions-pointer.previous.json', 'extension-migration-info.json', 'Backups/Extensions/Migrations/private.json'])
    assert.equal(isExtensionManagedPath(join(root, name), root), true, name);
});

test('invalid layout versions, UTF8, duplicate fields and occupied paths fail before creating missing framework', async t => {
  const home = await temporary(t);
  for (const [name, metadata] of [['future', '{"version":2}'], ['duplicate', '{"version":0,"version":1}'],
    ['utf8', Buffer.from([0x7b, 0xff, 0x7d])], ['null', 'null']]) {
    const root = join(home, name); await mkdir(root); await writeFile(join(root, 'extension-layout.json'), metadata);
    const before = await readFile(join(root, 'extension-layout.json'));
    await assert.rejects(ensureExtensionLayout(root), error => ['INVALID_EXTENSION_LAYOUT', 'UNSUPPORTED_EXTENSION_LAYOUT'].includes(error.code));
    assert.deepEqual(await readFile(join(root, 'extension-layout.json')), before);
    await assert.rejects(access(join(root, 'Agent')), { code: 'ENOENT' });
  }
  const root = join(home, 'occupied'); await mkdir(root); await writeFile(join(root, 'Skills'), 'keep');
  await assert.rejects(ensureExtensionLayout(root), { code: 'INVALID_EXTENSION_LAYOUT' });
  assert.equal(await readFile(join(root, 'Skills'), 'utf8'), 'keep');
  await assert.rejects(access(join(root, 'Agent')), { code: 'ENOENT' });
});

test('maintenance blocks initialization and interrupted scaffolding safely completes after release', async t => {
  const home = await temporary(t), root = join(home, 'during-maintenance');
  await assert.rejects(ensureExtensionLayout(root, { maintenanceActive: () => true }), { code: 'STORAGE_MAINTENANCE_ACTIVE' });
  await assert.rejects(access(root), { code: 'ENOENT' });
  let checks = 0;
  await assert.rejects(ensureExtensionLayout(root, { maintenanceActive: () => ++checks >= 4 }), { code: 'STORAGE_MAINTENANCE_ACTIVE' });
  await assert.rejects(access(join(root, 'extension-layout.json')), { code: 'ENOENT' });
  await ensureExtensionLayout(root);
  for (const name of EXTENSION_LAYOUT_DIRECTORIES) assert.equal((await stat(join(root, name))).isDirectory(), true);
});

test('layout initialization rejects linked framework directories and hard-linked control files', async t => {
  const home = await temporary(t), real = join(home, 'real'), root = join(home, 'Extensions');
  await mkdir(real); await mkdir(root);
  await symlink(real, join(root, 'MCP'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(ensureExtensionLayout(root), { code: 'INVALID_EXTENSION_STORAGE' });
  await assert.rejects(access(join(root, 'Agent')), { code: 'ENOENT' });
  await unlink(join(root, 'MCP'));
  const shared = join(real, 'layout.json'); await writeFile(shared, '{"version":1}');
  await link(shared, join(root, 'extension-layout.json'));
  await assert.rejects(ensureExtensionLayout(root), { code: 'INVALID_EXTENSION_STORAGE' });
  assert.equal(await readFile(shared, 'utf8'), '{"version":1}');
});

test('metadata activation keeps a concurrently-created future version and cleans its staging file on failure', async t => {
  const home = await temporary(t), root = join(home, 'concurrent-metadata'), metadata = join(root, 'extension-layout.json');
  let supplied = false;
  await assert.rejects(ensureExtensionLayout(root, { maintenanceActive: () => {
    if (!supplied && existsSync(join(root, 'Backups', 'Extensions', 'Migrations'))) {
      writeFileSync(metadata, '{"version":99,"owner":"concurrent"}', { flag: 'wx' }); supplied = true;
    }
    return false;
  } }), { code: 'UNSUPPORTED_EXTENSION_LAYOUT' });
  assert.equal(supplied, true);
  assert.equal(await readFile(metadata, 'utf8'), '{"version":99,"owner":"concurrent"}');
  assert.equal((await readdir(root)).some(name => name.startsWith('.extension-layout.')), false);
  await rm(metadata);
  let staged = false;
  await assert.rejects(ensureExtensionLayout(root, { maintenanceActive: () => {
    // The second check follows the complete temporary write and precedes publication.
    // 第二次检查位于完整的临时写入之后、正式发布之前。
    if (existsSync(root) && !staged) staged = readdirSync(root).some(name => name.startsWith('.extension-layout.'));
    return staged;
  } }), { code: 'STORAGE_MAINTENANCE_ACTIVE' });
  assert.equal(staged, true);
  await assert.rejects(access(metadata), { code: 'ENOENT' });
  assert.equal((await readdir(root)).some(name => name.startsWith('.extension-layout.')), false);
  await ensureExtensionLayout(root);
  assert.deepEqual(inspectExtensionLayout(root), { version: 1 });
});
