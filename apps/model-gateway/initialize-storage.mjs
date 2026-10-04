import { isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ConversationStore } from './conversations.mjs';
import { DATA_LAYOUT_VERSION } from './data-layout.mjs';

/**
 * Offline preparation of an inactive migration destination; never changes the active pointer.
 * 离线准备尚未激活的迁移目标，不修改当前生效的路径指针。
 */
export async function initializeStorage(target) {
  if (typeof target !== 'string' || !isAbsolute(target)) throw new Error('目标数据目录必须是绝对路径。');
  const root = resolve(target);
  const store = new ConversationStore({ root, dataHome: join(root, 'Models'), legacyDesktopDirectory: join(root, 'Desktop') });
  const catalog = await store.catalog();
  return { layoutVersion: DATA_LAYOUT_VERSION, projects: catalog.Projects.length,
    chats: catalog.Chats.length + catalog.Projects.reduce((count, project) => count + project.Chats.length, 0) };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { console.log(JSON.stringify(await initializeStorage(process.argv[2]))); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
