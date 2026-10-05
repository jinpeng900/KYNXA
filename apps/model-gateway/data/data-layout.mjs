import { randomUUID } from 'node:crypto';
import { lstat, mkdir, readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { atomicJson } from '../platform/atomic-json.mjs';
import { rebuildConversationIndex, inspectConversationIndex } from './conversation-index.mjs';

export const DATA_LAYOUT_VERSION = 1;
const rootDirectories = ['Projects', 'Chats', 'Memory', 'Knowledge', 'Retrieval', 'Index', 'Trash', 'Backups', 'Agent', 'Skills'];

function layoutError(message) {
  return Object.assign(new Error(message), { code: 'INVALID_DATA_LAYOUT', statusCode: 409 });
}

async function inspect(path, directory) {
  try {
    const info = await lstat(path);
    if (info.isSymbolicLink() || (directory ? !info.isDirectory() : !info.isFile()))
      throw layoutError('数据目录结构无效或包含链接，请检查存储位置。');
    return true;
  } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
}

async function directory(path) {
  if (!await inspect(path, true)) await mkdir(path, { recursive: true, mode: 0o700 });
}

async function jsonFile(path) {
  if (!await inspect(path, false)) return null;
  try {
    const value = JSON.parse((await readFile(path, 'utf8')).replace(/^\uFEFF/, ''));
    if (value === null) throw layoutError('存储设置格式无效，原文件已保留。');
    return value;
  }
  catch (error) {
    if (error instanceof SyntaxError) throw layoutError('存储设置文件格式无效，原文件已保留。');
    throw error;
  }
}

function identifier(value) {
  if (typeof value !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(value) ||
      /^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/i.test(value)) throw layoutError('项目或聊天 ID 无效。');
  return value.toLowerCase();
}

/**
 * Validate the version BEFORE any canonical migration or transaction is applied.
 * 正式迁移或事务执行前，必须先验证版本。
 */
export async function inspectDataLayout(root, document) {
  root = resolve(root);
  await inspect(root, true);
  const current = await jsonFile(join(root, 'settings.json'));
  if (current !== null && (!current || typeof current !== 'object' || Array.isArray(current)))
    throw layoutError('存储设置格式无效，原文件已保留。');
  const storage = current?.Storage;
  if (storage !== undefined && (!storage || typeof storage !== 'object' || Array.isArray(storage)))
    throw layoutError('存储版本设置格式无效。');
  if (storage?.LayoutVersion !== undefined && storage.LayoutVersion !== DATA_LAYOUT_VERSION)
    throw layoutError('此数据目录的结构版本不受当前程序支持，请使用兼容版本打开。');
  for (const name of rootDirectories) await inspect(join(root, name), true);
  await inspectConversationIndex(root);
  if (document) {
    for (const project of document.Projects) {
      const folder = join(root, 'Projects', identifier(project.Id));
      for (const path of [folder, join(folder, 'Sessions'), join(folder, 'Memory')]) await inspect(path, true);
      await inspect(join(folder, 'project.json'), false);
      for (const chat of project.Chats) {
        const session = join(folder, 'Sessions', identifier(chat.Id));
        await inspect(session, true);
        await inspect(join(session, 'attachments'), true);
        await inspect(join(session, 'Memory'), true);
        await inspect(join(session, 'context.json'), false);
        await inspect(join(session, 'events.jsonl'), false);
      }
    }
    for (const chat of document.Chats) {
      const session = join(root, 'Chats', identifier(chat.Id));
      await inspect(session, true);
      await inspect(join(session, 'attachments'), true);
      await inspect(join(session, 'Memory'), true);
      await inspect(join(session, 'context.json'), false);
      await inspect(join(session, 'events.jsonl'), false);
    }
  }
  return current;
}

/**
 * Reconcile only app-owned scaffolding. catalog.json remains the transactional
 * metadata authority; project.json is its portable, rebuildable manifest.
 * No workspace contents or user settings are inferred from absolute paths.
 * 仅校正应用拥有的目录框架；catalog.json 保持事务元数据权威，project.json 只是可移植、可重建的清单，不从绝对路径推断工作区内容或用户设置。
 */
export async function ensureDataLayout(root, document) {
  root = resolve(root);
  const current = await inspectDataLayout(root, document);
  await directory(root);
  for (const name of rootDirectories) await directory(join(root, name));

  for (let order = 0; order < document.Projects.length; order++) {
    const project = document.Projects[order];
    const folder = join(root, 'Projects', identifier(project.Id));
    await directory(folder);
    await directory(join(folder, 'Sessions'));
    await directory(join(folder, 'Memory'));
    const path = join(folder, 'project.json');
    await inspect(path, false);
    const manifest = {
      Version: DATA_LAYOUT_VERSION, Source: '../../catalog.json',
      Id: project.Id, Name: project.Name, FolderPath: project.FolderPath ?? null,
      Order: order, IsPinned: Boolean(project.IsPinned), IsArchived: Boolean(project.IsArchived),
      IsFolderlessWorkspace: Boolean(project.IsFolderlessWorkspace),
      Sessions: project.Chats.map(chat => chat.Id)
    };
    let previous;
    try { previous = await readFile(path, 'utf8'); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    const encoded = JSON.stringify(manifest, null, 2);
    if (previous?.trim() !== encoded) await atomicJson(path, manifest);
    for (const chat of project.Chats) {
      const session = join(folder, 'Sessions', identifier(chat.Id));
      await directory(session);
      await directory(join(session, 'attachments'));
      await directory(join(session, 'Memory'));
    }
  }
  for (const chat of document.Chats) {
    const session = join(root, 'Chats', identifier(chat.Id));
    await directory(session);
    await directory(join(session, 'attachments'));
    await directory(join(session, 'Memory'));
  }

  // Derived index uses actual SQLite; it can be reconstructed from the catalog.
  // 派生索引使用实际 SQLite，可由正式目录重建。
  await rebuildConversationIndex(root, document);
  const settings = { ...current, Storage: { ...current?.Storage, LayoutVersion: DATA_LAYOUT_VERSION,
    StoreId: current?.Storage?.StoreId ?? randomUUID(),
    CreatedAt: current?.Storage?.CreatedAt ?? new Date().toISOString() } };
  if (JSON.stringify(current) !== JSON.stringify(settings)) await atomicJson(join(root, 'settings.json'), settings);
}
