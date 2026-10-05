import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { runStorageMigration } from './data/migrate-storage.mjs';

export { migrateStorage, runStorageMigration } from './data/migrate-storage.mjs';

// Keep the operator migration CLI separate from ordinary runtime initialization.
// 操作者的迁移 CLI 与普通运行时初始化保持分离。
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await runStorageMigration();
