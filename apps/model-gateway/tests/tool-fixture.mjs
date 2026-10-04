import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { ConversationStore } from '../conversations.mjs';
import { ToolService } from '../tool-service.mjs';

export async function toolFixture(t, { sandboxRunner, desktopRunner, hostTerminalRunner, approvalTimeoutMs, nestedData = false, legacyData = false, officialTools = false } = {}) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-tool-test-'));
  const workspace = join(root, 'work');
  const dataHome = join(nestedData ? workspace : root, 'Data', legacyData ? 'custom-model-home' : 'Models');
  await mkdir(workspace, { recursive: true });
  const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
  const conversationId = randomUUID(), projectId = randomUUID(), standaloneId = randomUUID();
  const chat = id => ({ Id: id, Title: 'Synthetic tool chat', Messages: [{ Id: randomUUID(), Role: 'user',
    Content: 'Synthetic saved message', Status: 'completed' }] });
  await conversations.saveCatalog({ ...(await conversations.catalog()),
    Projects: [{ Id: projectId, Name: 'Synthetic tools work', FolderPath: workspace, Chats: [chat(conversationId)] }], Chats: [chat(standaloneId)] });
  const service = new ToolService({ conversationStore: conversations, dataHome, sandboxRunner, desktopRunner, hostTerminalRunner, approvalTimeoutMs, bundledDirectory: officialTools ? undefined : null, officialTools });
  t.after(async () => {
    await service.close();
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`), 'cleanup remains within owned temporary data');
    await rm(root, { recursive: true, force: true });
  });
  const context = (permissionMode = 'ask', id = conversationId) => service.createContext(id, { requestId: randomUUID(), permissionMode });
  const call = (name, args) => ({ id: randomUUID(), name, arguments: args });
  const run = (ctx, name, args, options = {}) => service.execute(ctx, call(name, args), options);
  return { root, workspace, dataHome, conversations, service, conversationId, projectId, standaloneId, context, call, run };
}

export function parsed(result) {
  assert.equal(result.isError, false, JSON.stringify(result));
  return JSON.parse(result.content);
}

export async function pendingApproval(service, context, call, options = {}) {
  let ready;
  const event = new Promise(resolve => { ready = resolve; });
  const result = service.execute(context, call, { ...options, emit: value => ready(value) });
  return { result, event: await event };
}

export function approve(service, context, tool, approved = true) {
  return service.approve({ conversationId: context.conversationId, requestId: context.requestId,
    toolCallId: tool.toolCallId, approvalId: tool.approvalId, approved });
}
