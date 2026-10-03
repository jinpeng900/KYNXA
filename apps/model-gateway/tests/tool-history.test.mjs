import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { parsed, toolFixture } from './tool-fixture.mjs';

test('current-chat history search covers over 200 messages, uses stable IDs and excludes private fields and tool records', async t => {
  const f = await toolFixture(t), context = await f.context('ask');
  const ids = [];
  for (let index = 0; index < 260; index++) {
    const id = randomUUID(); ids.push(id);
    await f.conversations.upsertMessage(context.conversationId, { Id: id, Role: index % 2 ? 'assistant' : 'user', Content: `needle public message ${index} 😀`,
      Reasoning: 'PRIVATE_REASONING_NEVER_SEARCHABLE', ToolActivities: [{ result: 'PRIVATE_RAW_TOOLS' }], AgentConfig: { hidden: 'PRIVATE_CONFIG' },
      _meta: { hidden: 'PRIVATE_METADATA' }, Status: 'completed' });
  }
  await f.conversations.upsertMessage(context.conversationId, { Id: randomUUID(), Role: 'tool', Content: 'needle PRIVATE_TOOL_RECORD', Status: 'completed' });
  const found = parsed(await f.run(context, 'conversation.history.search', { query: 'needle', offset: 240, limit: 20 }, { interactive: false }));
  assert.equal(found.total, 260); assert.equal(found.messages.length, 20); assert.equal(found.hasMore, false);
  assert.equal(found.messages[0].messageId, ids[240]); assert.equal(found.messages.at(-1).messageId, ids[259]);
  assert.ok(!JSON.stringify(found).includes('PRIVATE_')); assert.equal(found.conversationId, context.conversationId);
  const page = parsed(await f.run(context, 'conversation.history.read', { messageId: ids[259], offset: 0, limit: 4096 }, { interactive: false }));
  assert.equal(page.text, 'needle public message 259 😀'); assert.equal(page.truncated, false); assert.equal(page.messageId, ids[259]);
  assert.ok(!JSON.stringify(page).includes('PRIVATE_'));
});

test('original long message text pages preserve UTF-16 boundaries and cannot read another conversation', async t => {
  const f = await toolFixture(t), context = await f.context('ask'), id = randomUUID(), original = '😀 quoted " original\n'.repeat(1800);
  await f.conversations.upsertMessage(context.conversationId, { Id: id, Role: 'assistant', Content: original, Reasoning: 'PRIVATE_REASONING', Status: 'completed' });
  let offset = 0, text = '', pages = 0;
  do {
    const page = parsed(await f.run(context, 'conversation.history.read', { messageId: id, offset, limit: 4001 }, { interactive: false }));
    text += page.text; offset = page.nextOffset; pages++;
    assert.ok(!/[\uD800-\uDBFF]$/.test(page.text)); if (!page.truncated) break;
  } while (true);
  assert.ok(pages > 5); assert.equal(text, original);
  const otherId = (await f.conversations.readMessages(f.standaloneId))[0].Id;
  assert.equal((await f.run(context, 'conversation.history.read', { messageId: otherId }, { interactive: false })).code, 'CONVERSATION_HISTORY_MESSAGE_NOT_FOUND');
  assert.equal((await f.run(context, 'conversation.history.search', { query: 'needle', conversationId: f.standaloneId }, { interactive: false })).isError, true);
});

test('history tools reject archived, concurrently archived and deleted chats instead of exposing stale snapshots', async t => {
  const f = await toolFixture(t), context = await f.context('ask');
  const originalRead = f.conversations.readMessages.bind(f.conversations);
  f.conversations.readMessages = async id => {
    const messages = await originalRead(id), catalog = await f.conversations.catalog();
    catalog.Projects[0].Chats[0].IsArchived = true;
    await f.conversations.saveCatalog(catalog); return messages;
  };
  assert.equal((await f.run(context, 'conversation.history.search', { query: '' }, { interactive: false })).code, 'CONVERSATION_HISTORY_ARCHIVED');
  f.conversations.readMessages = originalRead;
  assert.equal((await f.run(context, 'conversation.history.search', {}, { interactive: false })).code, 'CONVERSATION_HISTORY_ARCHIVED');
  let catalog = await f.conversations.catalog(); catalog.Projects[0].Chats[0].IsArchived = false; catalog.Projects[0].IsArchived = true;
  await f.conversations.saveCatalog(catalog);
  assert.equal((await f.run(context, 'conversation.history.search', {}, { interactive: false })).code, 'CONVERSATION_HISTORY_ARCHIVED');
  catalog = await f.conversations.catalog(); catalog.Projects[0].Chats = [];
  await f.conversations.saveCatalog(catalog);
  assert.equal((await f.run(context, 'conversation.history.search', {}, { interactive: false })).code, 'CONVERSATION_DELETED');
});

test('search navigation offsets refer to original Unicode text and oversized pages are rejected', async t => {
  const f = await toolFixture(t), context = await f.context('ask'), id = randomUUID();
  await f.conversations.upsertMessage(context.conversationId, { Id: id, Role: 'user', Content: 'İ before needle', Status: 'completed' });
  const found = parsed(await f.run(context, 'conversation.history.search', { query: 'NEEDLE' }, { interactive: false }));
  assert.equal(found.messages[0].matchOffset, 'İ before '.length);
  assert.equal(found.messages[0].messageId, id);
  const expanded = parsed(await f.run(context, 'conversation.history.search', { query: '\u0307' }, { interactive: false }));
  assert.equal(expanded.messages[0].matchOffset, 0, 'a case-folded expansion still points at the original character');
  assert.equal((await f.run(context, 'conversation.history.search', { query: 'NEEDLE', offset: 9 }, { interactive: false })).code, 'INVALID_TOOL_ARGUMENTS');
  assert.equal((await f.run(context, 'conversation.history.read', { messageId: id, offset: 100 }, { interactive: false })).code, 'INVALID_TOOL_ARGUMENTS');
});
