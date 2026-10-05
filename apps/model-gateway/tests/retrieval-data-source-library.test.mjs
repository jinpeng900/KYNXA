import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { ConversationStore } from '../data/conversations.mjs';
import { SourceLibrary } from '../data/retrieval/source-library.mjs';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-retrieval-library-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const conversations = new ConversationStore({ root, dataHome: join(root, 'Models'), legacyDesktopDirectory: null });
  const catalog = await conversations.catalog();
  catalog.Projects.push({ Id: 'work-a', Name: 'Synthetic work', FolderPath: join(root, 'synthetic-work'), Chats: [] });
  await conversations.saveCatalog(catalog);
  const library = new SourceLibrary({ root, conversationStore: conversations });
  return { root, conversations, library };
}

test('single-source reads enforce authorized scope, revision, deletion and actual snapshot hash', async t => {
  const { root, library } = await fixture(t);
  const added = await library.add([{ path: 'synthetic-public.md', title: 'Public document', text: 'verified public text' }]);
  const project = await library.add([{ path: 'synthetic-work.md', title: 'Project document', text: 'verified project text' }],
    { scope: 'project', projectId: 'work-a' });
  const entry = added.sources[0], projectEntry = project.sources[0];
  const snapshot = await library.readSource(entry.id, { scopeKeys: ['user'], sourceRevision: 1 });
  assert.equal(snapshot.text, 'verified public text');
  assert.equal(snapshot.sourceId, entry.id);
  assert.equal(await library.readSource(projectEntry.id, { scopeKeys: ['user'] }), null);
  assert.equal(await library.readSource(entry.id, { scopeKeys: ['user'], sourceRevision: 2 }), null);
  assert.throws(() => library.readSource(entry.id, { scopeKeys: [] }), { code: 'RETRIEVAL_SCOPE_REQUIRED' });
  const catalogBefore = await readFile(join(root, 'Knowledge', 'catalog.json'), 'utf8');
  await writeFile(join(root, 'Knowledge', entry.id, 'source', 'document.txt'), 'modified outside registry', 'utf8');
  assert.equal(await library.readSource(entry.id, { scopeKeys: ['user'], sourceRevision: 1 }), null);
  assert.equal((await library.list()).sources.find(value => value.id === entry.id).error, 'STALE_RETRIEVAL_SOURCE');
  assert.equal(await readFile(join(root, 'Knowledge', 'catalog.json'), 'utf8'), catalogBefore);
  assert.equal((await library.readAll(['user', 'project:work-a'])).length, 1);
  await writeFile(join(root, 'Knowledge', entry.id, 'source', 'document.txt'), 'verified public text', 'utf8');
  assert.equal((await library.readSource(entry.id, { scopeKeys: ['user'] })).text, snapshot.text);
  assert.equal((await library.list()).sources.find(value => value.id === entry.id).status, 'ready');
  await library.remove(entry.id, { expectedRevision: 1 });
  assert.equal(await library.readSource(entry.id, { scopeKeys: ['user'] }), null);
});

test('project descriptions expose the folder without loading sibling messages and archived sources stay unavailable', async t => {
  const { conversations, library, root } = await fixture(t);
  const relationship = await conversations.describeProject('work-a');
  assert.deepEqual(relationship, { projectId: 'work-a', name: 'Synthetic work', folderPath: join(root, 'synthetic-work'),
    isFolderlessWorkspace: false, isArchived: false });
  const added = await library.add([{ path: 'synthetic-work.md', text: 'project evidence' }], { scope: 'project', projectId: 'work-a' });
  const catalog = await conversations.catalog();
  catalog.Projects[0].IsArchived = true;
  await conversations.saveCatalog(catalog);
  assert.equal(await library.readSource(added.sources[0].id, { scopeKeys: ['project:work-a'], sourceRevision: 1 }), null);
  assert.equal((await conversations.describeProject('work-a')).isArchived, true);
});
