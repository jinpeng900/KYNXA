import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { initializeStorage } from './data/initialize-storage.mjs';

export { initializeStorage } from './data/initialize-storage.mjs';

// Keep migration preparation available at its installed CLI path.
// 保留迁移准备程序的安装包 CLI 路径。
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { console.log(JSON.stringify(await initializeStorage(process.argv[2]))); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
