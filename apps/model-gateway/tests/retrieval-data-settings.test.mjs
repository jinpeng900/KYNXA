import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { RetrievalSettingsStore } from '../data/retrieval/settings.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-settings-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ root, dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const settings = new RetrievalSettingsStore({ root, conversationStore: conversations });
  const catalog = await conversations.catalog();
  catalog.Projects.push({ Id: 'work-a', Name: 'Synthetic work', FolderPath: null, Chats: [] },
    { Id: 'folderless', Name: 'Synthetic folderless work', FolderPath: null, Chats: [], IsFolderlessWorkspace: true });
  await conversations.saveCatalog(catalog);
  return { root, conversations, settings };
}

test('global defaults are lazy, local-only and bounded; CAS persists through restart', async t => {
  const { root, settings, conversations } = await fixture(t);
  const initial = await settings.getGlobal();
  assert.equal(initial.local.embeddingProfileId, 'builtin-multilingual');
  assert.equal(initial.web.depth, 'standard');
  assert.equal(initial.revision, 0);
  await assert.rejects(stat(join(root, 'Retrieval', 'settings.json')), { code: 'ENOENT' });
  const attempts = await Promise.allSettled([
    settings.patchGlobal({ expectedRevision: 0, patch: { web: { mode: 'off' } } }),
    settings.patchGlobal({ expectedRevision: 0, patch: { local: { enabled: false } } })
  ]);
  assert.equal(attempts.filter(item => item.status === 'fulfilled').length, 1);
  assert.equal(attempts.find(item => item.status === 'rejected').reason.code, 'RETRIEVAL_SETTINGS_CONFLICT');
  const restarted = new RetrievalSettingsStore({ conversationStore: conversations });
  assert.equal((await restarted.getGlobal()).revision, 1);
  assert.equal((await conversations.catalog()).Chats.length, 0);
});

test('project overrides inherit global settings, clear explicitly and cannot supply permissions or binding revisions', async t => {
  const { settings } = await fixture(t);
  assert.equal((await settings.getEffective('work-a')).projectIndexing.mountedFolder, false);
  await settings.patchGlobal({ expectedRevision: 0, patch: { web: { depth: 'deep' } } });
  const project = await settings.patchProject('work-a', { expectedRevision: 0,
    patch: { overrides: { web: { mode: 'off' } }, indexingSources: { mountedFolder: { enabled: true } } } });
  assert.equal(project.indexingSources.mountedFolder.bindingRevision, 1);
  const effective = await settings.getEffective('work-a');
  assert.equal(effective.web.mode, 'off');
  assert.equal(effective.web.depth, 'deep');
  assert.equal(effective.projectIndexing.mountedFolder, true);
  const cleared = await settings.patchProject('work-a', { expectedRevision: 1, patch: { overrides: { web: null } } });
  assert.deepEqual(cleared.overrides, {});
  assert.equal((await settings.getEffective('work-a')).web.mode, 'auto');
  assert.equal((await settings.getEffective(null)).projectIndexing.mountedFolder, false);
  assert.throws(() => settings.patchProject('work-a', { expectedRevision: 2,
    patch: { indexingSources: { mountedFolder: { enabled: true, bindingRevision: 999 } } } }), { code: 'INVALID_RETRIEVAL_INPUT' });
  await assert.rejects(() => settings.patchProject('folderless', { expectedRevision: 0,
    patch: { indexingSources: { mountedFolder: { enabled: true } } } }), { code: 'RETRIEVAL_FOLDER_UNAVAILABLE' });
  await assert.rejects(() => settings.getProject('missing-project'), { code: 'PROJECT_NOT_FOUND' });
});

test('unknown fields, executable references, large cache budgets and future configuration preserve existing files', async t => {
  const { root, settings } = await fixture(t);
  assert.throws(() => settings.patchGlobal({ expectedRevision: 0, patch: { permissionMode: 'full' } }), { code: 'INVALID_RETRIEVAL_INPUT' });
  assert.throws(() => settings.patchGlobal({ expectedRevision: 0, patch: { local: { nativeDll: 'untrusted.dll' } } }), { code: 'INVALID_RETRIEVAL_INPUT' });
  assert.throws(() => settings.patchGlobal({ expectedRevision: 0, patch: { cache: { memoryLimitBytes: 1024 * 1024 * 1024 } } }), { code: 'INVALID_RETRIEVAL_INPUT' });
  await settings.patchGlobal({ expectedRevision: 0, patch: { local: { semantic: 'off' } } });
  const filename = join(root, 'Retrieval', 'settings.json');
  const future = { ...await settings.getGlobal(), schemaVersion: 99 };
  await writeFile(filename, JSON.stringify(future), 'utf8');
  await assert.rejects(() => settings.getGlobal(), { code: 'UNSUPPORTED_RETRIEVAL_SETTINGS_VERSION' });
  await assert.rejects(() => settings.patchGlobal({ expectedRevision: 1, patch: { web: { depth: 'deep' } } }), { code: 'UNSUPPORTED_RETRIEVAL_SETTINGS_VERSION' });
  assert.equal(JSON.parse(await readFile(filename, 'utf8')).schemaVersion, 99);
});
