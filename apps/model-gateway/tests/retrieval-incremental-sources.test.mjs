import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { mkdtemp, mkdir, readFile, rename, rm, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { join, relative, resolve, sep } from 'node:path';
import { SourceSyncService } from '../orchestration/retrieval/source-sync.mjs';
import { SourceManifestStore } from '../data/retrieval/source-manifest.mjs';
import { readSourceTree, scanSourceTree } from '../tools/retrieval/source-reader.mjs';

async function fixture(t, { unavailableWatcher = false, onFolderChanged, dataInsideWorkspace = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-incremental-'));
  const workspace = join(root, 'workspace'), dataRoot = dataInsideWorkspace ? join(workspace, 'managed-data') : join(root, 'data');
  await mkdir(workspace); await mkdir(dataRoot);
  const project = { FolderPath: workspace };
  const settings = { local: { indexing: { maximumFiles: 20000, maximumSourceBytes: 2097152,
    maximumTotalBytes: 536870912, maximumEntries: 200000, batchSize: 32 } },
    cache: { memoryLimitBytes: 16 * 1024 * 1024 }, projectIndexing: { mountedFolder: true, bindingRevision: 1 } };
  const services = [];
  const notifications = [];
  const create = () => {
    const service = new SourceSyncService({ library: { root: dataRoot }, getProject: async () => project,
      excludedRoots: [dataRoot], onFolderChanged, watchFactory: (_path, _options, listener) => {
        if (unavailableWatcher) throw Object.assign(new Error('Fixture watcher unavailable.'), { code: 'ENOSYS' });
        notifications.push(listener);
        const watcher = new EventEmitter(); watcher.close = () => {}; return watcher;
      } });
    services.push(service); return service;
  };
  t.after(async () => {
    for (const service of services) service.close();
    const suffix = relative(resolve(tmpdir()), root);
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, dataRoot, workspace, project, settings, create,
    ownService: service => { services.push(service); return service; },
    emitChange: filename => notifications.at(-1)?.('change', filename) };
}

test('durable mounted metadata skips unchanged file bodies across scans and restart', async t => {
  const f = await fixture(t);
  for (let index = 0; index < 8; index++) await writeFile(join(f.workspace, `source-${index}.cs`), `class Source${index} {}`);
  const firstService = f.create();
  const first = await firstService.mountedSnapshot('project-a', f.settings);
  assert.equal(first.scan.fileReads, 8);
  assert.equal(first.scan.reusedFiles, 0);
  assert.ok(first.sources.every(source => source.text === undefined));
  assert.ok(first.sources.every(source => Number.isSafeInteger(source.storedBytes) && source.storedBytes > 0));
  assert.equal(await first.isCurrent(first.sources[0]), true);
  assert.equal((await first.loadSource(first.sources[0])).text, 'class Source0 {}');
  firstService.mountedCache.clear();
  const repeated = await firstService.mountedSnapshot('project-a', f.settings);
  assert.equal(repeated.scan.fileReads, 0);
  assert.equal(repeated.scan.reusedFiles, 8);
  firstService.close();
  const restarted = await f.create().mountedSnapshot('project-a', f.settings);
  assert.equal(restarted.scan.fileReads, 0);
  assert.equal(restarted.scan.reusedFiles, 8);
  assert.deepEqual(restarted.sources.map(source => source.sourceId), first.sources.map(source => source.sourceId));
  const store = new SourceManifestStore(f.dataRoot);
  const manifest = await store.read({ projectId: 'project-a', root: f.workspace, bindingRevision: 1 });
  assert.equal(manifest.files.length, 8);
  assert.ok(manifest.files.every(file => !Object.hasOwn(file, 'text')));
});

test('dirty reconciliation reads only changed files and updates deletion and rename identities', async t => {
  const f = await fixture(t), service = f.create();
  for (let index = 0; index < 6; index++) await writeFile(join(f.workspace, `file-${index}.md`), `Original content ${index}.`);
  const first = await service.mountedSnapshot('project-a', f.settings);
  await writeFile(join(f.workspace, 'file-2.md'), 'Updated content two.');
  service.markChanged('project-a', 'file-2.md');
  const updated = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(updated.scan.fileReads, 1);
  assert.equal(updated.scan.visitedEntries, 1);
  assert.equal(updated.sources.length, 6);
  assert.notEqual(updated.sources.find(source => source.title === 'file-2.md').contentHash,
    first.sources.find(source => source.title === 'file-2.md').contentHash);
  await rename(join(f.workspace, 'file-3.md'), join(f.workspace, 'renamed.md'));
  await unlink(join(f.workspace, 'file-4.md'));
  service.markChanged('project-a', 'file-3.md'); service.markChanged('project-a', 'renamed.md');
  service.markChanged('project-a', 'file-4.md');
  const moved = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(moved.scan.fileReads, 1);
  assert.equal(moved.sources.length, 5);
  assert.ok(!moved.sources.some(source => ['file-3.md', 'file-4.md'].includes(source.title)));
  assert.ok(moved.sources.some(source => source.title === 'renamed.md'));
});

test('ignore and unknown directory changes perform complete metadata reconciliation', async t => {
  const f = await fixture(t), service = f.create();
  await mkdir(join(f.workspace, 'nested'));
  await writeFile(join(f.workspace, 'nested', 'note.md'), 'Included content.');
  await writeFile(join(f.workspace, 'root.md'), 'Root content.');
  await service.mountedSnapshot('project-a', f.settings);
  await writeFile(join(f.workspace, '.gitignore'), 'nested/\n');
  service.markChanged('project-a', '.gitignore');
  const ignored = await service.mountedSnapshot('project-a', f.settings);
  assert.deepEqual(ignored.sources.map(source => source.title), ['root.md']);
  assert.equal(ignored.scan.fileReads, 0);
  await writeFile(join(f.workspace, '.gitignore'), '');
  service.markChanged('project-a', undefined);
  const restored = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(restored.sources.length, 2);
  assert.equal(restored.scan.fileReads, 1);
  await mkdir(join(f.workspace, 'new-folder'));
  await writeFile(join(f.workspace, 'new-folder', 'new.cs'), 'class NewlyAdded {}');
  service.markChanged('project-a', 'new-folder');
  const directory = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(directory.sources.length, 3);
  assert.equal(directory.scan.fileReads, 1);
  const removableFolder = resolve(f.workspace, 'new-folder');
  assert.equal(relative(f.workspace, removableFolder), 'new-folder');
  await rm(removableFolder, { recursive: true });
  service.markChanged('project-a', 'new-folder');
  const deletedDirectory = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(deletedDirectory.sources.length, 2);
  assert.ok(!deletedDirectory.sources.some(source => source.title.includes('new-folder')));
});

test('descriptor hydration rejects changed content and moved project bindings', async t => {
  const f = await fixture(t), service = f.create();
  const path = join(f.workspace, 'job.cs');
  await writeFile(path, 'class Original {}');
  const first = await service.mountedSnapshot('project-a', f.settings);
  await writeFile(path, 'class Modified {}');
  assert.equal(await first.isCurrent(first.sources[0]), false);
  await assert.rejects(first.loadSource(first.sources[0]), { code: 'STALE_RETRIEVAL_SOURCE' });
  f.project.FolderPath = join(f.root, 'other-workspace');
  await assert.rejects(first.loadSource(first.sources[0]), { code: 'STALE_RETRIEVAL_SOURCE' });
});

test('polling fallback and cancellation are explicit and preserve the previous manifest', async t => {
  const f = await fixture(t, { unavailableWatcher: true }), service = f.create();
  await writeFile(join(f.workspace, 'one.md'), 'Initial content.');
  await service.mountedSnapshot('project-a', f.settings);
  assert.deepEqual(service.status().watchers, [{ state: 'polling', diagnosticCode: 'ENOSYS' }]);
  assert.equal(service.status().reconciliationIntervalMs, 30000);
  const store = new SourceManifestStore(f.dataRoot), binding = { projectId: 'project-a', root: f.workspace, bindingRevision: 1 };
  const original = await readFile(store.pathFor(binding), 'utf8');
  const cancelled = new AbortController(); cancelled.abort();
  await assert.rejects(service.mountedSnapshot('project-a', f.settings, cancelled.signal), { name: 'AbortError' });
  assert.equal(await readFile(store.pathFor(binding), 'utf8'), original);
  service.close();
  await assert.rejects(service.mountedSnapshot('project-a', f.settings), { code: 'RETRIEVAL_CLOSED' });
});

test('streaming scan budgets and existing array import behavior remain compatible', async t => {
  const f = await fixture(t);
  await writeFile(join(f.workspace, 'one.md'), 'First source.');
  await writeFile(join(f.workspace, 'two.cs'), 'class Second {}');
  await writeFile(join(f.workspace, 'binary.txt'), Buffer.from([0, 1, 2]));
  const files = await readSourceTree(f.workspace);
  assert.equal(files.length, 2);
  assert.ok(files.every(file => typeof file.text === 'string'));
  await assert.rejects(readSourceTree(f.workspace, { maximumFiles: 1 }), { code: 'RETRIEVAL_SCAN_LIMIT' });
  const previousFiles = new Map(files.map(file => [file.title, file]));
  const stats = {}, descriptors = [];
  for await (const file of scanSourceTree(f.workspace, { previousFiles, stats })) descriptors.push(file);
  assert.equal(stats.fileReads, 0);
  assert.equal(stats.reusedFiles, 2);
  assert.ok(descriptors.every(file => file.text === undefined));
});

test('lowered budgets invalidate hot mounted snapshots and force complete resource validation', async t => {
  const f = await fixture(t), service = f.create();
  await writeFile(join(f.workspace, 'small.md'), 'Small.');
  await writeFile(join(f.workspace, 'large.cs'), 'class LargerFile {}');
  const initial = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(initial.sources.length, 2);
  f.settings.local.indexing.maximumFiles = 1;
  await assert.rejects(service.mountedSnapshot('project-a', f.settings), { code: 'RETRIEVAL_SCAN_LIMIT' });
  f.settings.local.indexing.maximumFiles = 20000;
  f.settings.local.indexing.maximumTotalBytes = 1;
  await assert.rejects(service.mountedSnapshot('project-a', f.settings), { code: 'RETRIEVAL_SCAN_LIMIT' });
  f.settings.local.indexing.maximumTotalBytes = 536870912;
  f.settings.local.indexing.maximumSourceBytes = 8;
  const smaller = await service.mountedSnapshot('project-a', f.settings);
  assert.deepEqual(smaller.sources.map(source => source.title), ['small.md']);
  assert.equal(smaller.scan.fileReads, 0);
});

test('polling fallback schedules reconciliation, invalidates snapshots, and stops after close', async t => {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  let callbacks = 0;
  const f = await fixture(t, { unavailableWatcher: true, onFolderChanged: () => { callbacks++; } });
  const service = f.create();
  await writeFile(join(f.workspace, 'one.md'), 'Original.');
  await service.mountedSnapshot('project-a', f.settings);
  await writeFile(join(f.workspace, 'one.md'), 'Changed.');
  t.mock.timers.tick(3000);
  service.mountedCache.get('project-a').capturedAt = performance.now() - 3000;
  const warm = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(warm.scan.cached, true);
  assert.equal(warm.scan.fileReads, 0);
  assert.equal(warm.scan.visitedEntries, 0);
  t.mock.timers.tick(27000);
  t.mock.timers.tick(800);
  await Promise.resolve();
  assert.equal(callbacks, 1);
  assert.equal(service.mountedCache.has('project-a'), false);
  const updated = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(updated.scan.fileReads, 1);
  service.close();
  t.mock.timers.tick(60000);
  await Promise.resolve();
  assert.equal(callbacks, 1);
});

test('library snapshots load only requested authorized bodies and reject revisions that changed', async () => {
  const source = { sourceId: 'source-a', sourceRevision: 2, contentHash: 'a'.repeat(64) };
  let reads = 0, revoked = false;
  const service = new SourceSyncService({ library: { describeSources: async () => ({ revision: 1, sources: [source] }),
    readSource: async (_id, options) => { reads++; assert.deepEqual(options.scopeKeys, ['user']);
      return revoked ? null : { ...source, text: 'Fixture body.' }; } }, getProject: async () => null });
  const snapshot = await service.librarySnapshot(['user'], { local: {}, cache: { memoryLimitBytes: 1024 } });
  assert.equal(reads, 0);
  assert.equal((await snapshot.loadSource(source)).text, 'Fixture body.');
  assert.equal(reads, 1);
  revoked = true;
  await assert.rejects(snapshot.loadSource(source), { code: 'STALE_RETRIEVAL_SOURCE' });
  service.close();
});

test('managed data notifications cannot reindex their own manifests or hide neighboring ordinary sources', async t => {
  t.mock.timers.enable({ apis: ['setTimeout', 'setInterval'] });
  let callbacks = 0;
  const f = await fixture(t, { dataInsideWorkspace: true, onFolderChanged: () => { callbacks++; } });
  const adjacentFolder = join(f.workspace, 'managed-data-extra');
  await mkdir(adjacentFolder);
  await writeFile(join(adjacentFolder, 'note.md'), 'Adjacent ordinary source.');
  await writeFile(join(f.workspace, 'source.md'), 'Original ordinary source.');
  await writeFile(join(f.dataRoot, 'excluded.md'), 'Excluded managed data fixture.');
  const service = f.create(), originalWrite = service.manifest.write.bind(service.manifest);
  service.manifest.write = async (binding, files, options) => {
    const receipt = await originalWrite(binding, files, options);
    const relativeManifest = relative(f.workspace, service.manifest.pathFor(binding));
    f.emitChange(relativeManifest);
    f.emitChange(`${relativeManifest}.fixture.tmp`);
    return receipt;
  };
  const first = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(first.sources.length, 2);
  t.mock.timers.tick(1000); await Promise.resolve();
  assert.equal(callbacks, 0, 'writing the metadata manifest never schedules a recursive rebuild');
  assert.equal(service.mountedCache.has('project-a'), true);
  await writeFile(join(f.workspace, 'source.md'), 'Changed ordinary source.');
  f.emitChange('source.md');
  t.mock.timers.tick(800); await Promise.resolve();
  assert.equal(callbacks, 1);
  const changed = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(changed.scan.fileReads, 1);
  assert.equal((await changed.loadSource(changed.sources.find(source => source.title === 'source.md'))).text, 'Changed ordinary source.');
  t.mock.timers.tick(1000); await Promise.resolve();
  assert.equal(callbacks, 1, 'the follow-up manifest write is excluded too');
  await writeFile(join(adjacentFolder, 'note.md'), 'Changed adjacent ordinary source.');
  f.emitChange(relative(f.workspace, join(adjacentFolder, 'note.md')));
  t.mock.timers.tick(800); await Promise.resolve();
  assert.equal(callbacks, 2, 'a similarly named neighboring directory remains observable');
  const adjacent = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(adjacent.scan.fileReads, 1);
  assert.equal((await adjacent.loadSource(adjacent.sources.find(source => source.title.includes('managed-data-extra')))).text,
    'Changed adjacent ordinary source.');
});

test('native recursive watching ignores nested managed manifest writes but observes ordinary file changes', async t => {
  const f = await fixture(t, { dataInsideWorkspace: true });
  const path = join(f.workspace, 'source.md');
  await writeFile(path, 'Initial native source.');
  let callbacks = 0;
  const service = f.ownService(new SourceSyncService({ library: { root: f.dataRoot }, getProject: async () => f.project,
    excludedRoots: [f.dataRoot], onFolderChanged: () => { callbacks++; } }));
  await service.mountedSnapshot('project-a', f.settings);
  if (service.status().watchers[0].state !== 'watching') {
    t.skip('Native recursive watching is unavailable; controlled notification and polling tests cover this platform.');
    return;
  }
  await delay(1100);
  assert.equal(callbacks, 0, 'real manifest writes must not cause background rebuilds');
  await writeFile(path, 'Updated native source.');
  const deadline = performance.now() + 2500;
  while (!callbacks && performance.now() < deadline) await delay(25);
  assert.equal(callbacks, 1);
  const changed = await service.mountedSnapshot('project-a', f.settings);
  assert.equal(changed.scan.fileReads, 1);
  assert.equal((await changed.loadSource(changed.sources[0])).text, 'Updated native source.');
  service.close();
});

test('foreground discovery does not await a slow scan and saved descriptors still require current source hashes', async t => {
  let scheduled = 0;
  const f = await fixture(t, { onFolderChanged: () => { scheduled++; } });
  const path = join(f.workspace, 'source.md');
  await writeFile(path, 'Original foreground source.');
  const service = f.create();
  const original = await service.mountedSnapshot('project-a', f.settings);
  let releaseScan;
  const gate = new Promise(resolveScan => { releaseScan = resolveScan; });
  const capture = service.captureMounted.bind(service);
  service.captureMounted = async (...args) => { await gate; return capture(...args); };
  const foreground = await service.foregroundMountedSnapshot('project-a', f.settings);
  assert.equal(foreground.scan.backgroundPending, true);
  assert.equal(foreground.sources[0].sourceId, original.sources[0].sourceId);
  assert.equal(scheduled, 0);
  await writeFile(path, 'Changed while discovery is running.');
  await assert.rejects(foreground.loadSource(foreground.sources[0]), { code: 'STALE_RETRIEVAL_SOURCE' });
  releaseScan();
  await Promise.all([...service.foregroundContinuations.values()]);
  assert.equal(scheduled, 1);
  assert.equal(service.foregroundContinuations.size, 0);
});

test('closing deferred discovery aborts its owned scan and prevents a late rebuild', async t => {
  let scheduled = 0;
  const f = await fixture(t, { onFolderChanged: () => { scheduled++; } });
  const service = f.create();
  let observedSignal;
  service.captureMounted = (_id, _settings, signal) => {
    observedSignal = signal;
    return new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
  };
  const foreground = await service.foregroundMountedSnapshot('project-a', f.settings);
  assert.equal(foreground.scan.backgroundPending, true);
  assert.equal(foreground.sources.length, 0);
  service.close();
  assert.equal(observedSignal.aborted, true);
  await Promise.allSettled([...service.foregroundContinuations.values()]);
  assert.equal(scheduled, 0);
  assert.equal(service.foregroundContinuations.size, 0);
});
