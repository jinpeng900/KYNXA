import { createHash } from 'node:crypto';
import { open, readFile, readdir, realpath } from 'node:fs/promises';
import { basename, isAbsolute, join, resolve } from 'node:path';
import { validateId } from './conversations.mjs';
import { ensureLocalDirectory, inspectLocalPath, toolFailure, within } from './tool-paths.mjs';

const workspaceQueues = new Map();
const UUID = /^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$/i;
const OWNER_FILE = '.workspace-owner.json';
const MAX_OWNER_BYTES = 1024;

function identity(conversationId) {
  const id = validateId(conversationId).toLowerCase();
  // Older formal IDs stay unchanged. A durable owner marker prevents a chosen UUID from claiming their hashed directory.
  // 旧正式 ID 保持不变；持久化归属标记避免任意 UUID 占用其他 ID 对应的哈希目录。
  const folderId = UUID.test(id) ? id : validateId(createHash('sha256')
    .update('kynxa-conversation-workspace/v1:' + id).digest('hex').slice(0, 32));
  return { id, folderId };
}

function absoluteLocalPath(path) {
  if (typeof path !== 'string' || !path || !isAbsolute(path) || /[\0\r\n]/u.test(path) ||
      (process.platform === 'win32' && /^\\\\/u.test(path)))
    throw toolFailure('聊天工具工作区必须是绝对本地目录。', 'INVALID_SANDBOX_WORKSPACE', 400);
  return resolve(path);
}

async function directory(path) {
  const existing = await inspectLocalPath(path, { allowMissing: true });
  if (existing && !existing.isDirectory())
    throw toolFailure('聊天工具工作区已被非目录占用。', 'UNSAFE_TOOL_PATH', 403);
  await ensureLocalDirectory(path);
  if (!(await inspectLocalPath(path)).isDirectory())
    throw toolFailure('聊天工具工作区结构无效。', 'UNSAFE_TOOL_PATH', 403);
}

/**
 * Persistent per-chat generated files, separate from formal messages and memory. No snapshot write-back or cleanup.
 * 每个聊天的生成文件持久保存，并与正式消息、记忆分开，不执行快照回写或自动清理。
 */
export class ConversationWorkspaces {
  constructor({ root } = {}) {
    this.root = absoluteLocalPath(root);
    this.folder = join(this.root, 'Workspaces');
    this.folderAliases = new Set([this.folder]);
  }

  _path(conversationId) {
    return join(this.folder, identity(conversationId).folderId);
  }

  /**
   * Owned scope metadata is not an editable generated artifact. Protect its aliases in the filesystem broker.
   * 范围归属元数据不是可编辑生成物，其路径别名也受文件代理保护。
   */
  isControlPath(path) {
    return typeof path === 'string' && isAbsolute(path) && [...this.folderAliases].some(folder => within(folder, path)) &&
      basename(path).toLowerCase() === OWNER_FILE;
  }

  async _verifyOwner(conversationId, workspace) {
    const path = join(workspace, OWNER_FILE);
    const info = await inspectLocalPath(path, { allowMissing: true });
    if (!info || !info.isFile() || info.size > MAX_OWNER_BYTES)
      throw toolFailure('聊天工具工作区缺少有效的归属记录。', 'SANDBOX_WORKSPACE_OWNER_MISMATCH', 409);
    let owner;
    try { owner = JSON.parse(await readFile(path, 'utf8')); }
    catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
      throw toolFailure('聊天工具工作区归属记录损坏。', 'SANDBOX_WORKSPACE_OWNER_MISMATCH', 409);
    }
    if (owner?.schemaVersion !== 1 || owner.conversationId !== identity(conversationId).id)
      throw toolFailure('聊天工具工作区属于其他聊天或版本不受支持。', 'SANDBOX_WORKSPACE_OWNER_MISMATCH', 409);
  }

  async _initializeOwner(conversationId, workspace) {
    const path = join(workspace, OWNER_FILE);
    if (await inspectLocalPath(path, { allowMissing: true })) return;
    if ((await readdir(workspace)).length)
      throw toolFailure('已有工具工作文件缺少归属记录，未接管此目录。', 'SANDBOX_WORKSPACE_OWNER_MISMATCH', 409);
    let handle;
    try {
      handle = await open(path, 'wx', 0o600);
      await handle.writeFile(JSON.stringify({ schemaVersion: 1, conversationId: identity(conversationId).id }) + '\n', 'utf8');
      await handle.sync();
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      // A concurrent owner must match; do not overwrite a marker or infer ownership from the directory name.
      // 并发创建者必须具有相同归属，不覆盖标记，也不从目录名称推断归属。
    } finally { await handle?.close(); }
  }

  /**
   * Lazily initialize one chat. Reopening returns the same files; no catalog ownership is created or changed.
   * 按需初始化单个聊天，重开复用同一批文件，不创建或改变目录中的正式归属。
   */
  async ensure(conversationId) {
    const path = this._path(conversationId), previous = workspaceQueues.get(path) ?? Promise.resolve();
    const pending = previous.catch(() => {}).then(async () => {
      await directory(this.root);
      await directory(this.folder);
      await directory(path);
      await this._initializeOwner(conversationId, path);
      return this.verify(conversationId, path);
    });
    workspaceQueues.set(path, pending);
    try { return await pending; }
    finally { if (workspaceQueues.get(path) === pending) workspaceQueues.delete(path); }
  }

  /**
   * Recheck the exact root before execution/approval. Children, siblings and link replacements never qualify.
   * 审批或执行前重新验证精确根目录，子目录、同级目录和链接替换均不符合条件。
   */
  async verify(conversationId, path) {
    const expected = this._path(conversationId), candidate = absoluteLocalPath(path);
    let existing;
    try { existing = await inspectLocalPath(expected); }
    catch (error) {
      if (error.code === 'ENOENT')
        throw toolFailure('聊天工具工作区不存在或已更改。', 'SANDBOX_WORKSPACE_CHANGED', 409);
      throw error;
    }
    if (!existing.isDirectory())
      throw toolFailure('聊天工具工作区已被非目录占用。', 'UNSAFE_TOOL_PATH', 403);
    const canonical = await realpath(expected), canonicalFolder = await realpath(this.folder);
    const canonicalExpected = join(canonicalFolder, identity(conversationId).folderId);
    if (!within(canonicalExpected, canonical) || !within(canonical, canonicalExpected))
      throw toolFailure('聊天工具工作区解析到其他目录。', 'UNSAFE_TOOL_PATH', 403);
    const sameRoot = within(expected, candidate) && within(candidate, expected);
    const sameCanonical = within(canonical, candidate) && within(candidate, canonical);
    if (!sameRoot && !sameCanonical)
      throw toolFailure('聊天工具工作区身份不匹配。', 'SANDBOX_WORKSPACE_MISMATCH', 403);
    await inspectLocalPath(canonical);
    this.folderAliases.add(canonicalFolder);
    await this._verifyOwner(conversationId, canonical);
    return canonical;
  }
}
