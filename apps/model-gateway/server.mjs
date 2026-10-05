import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { startModelServer } from './orchestration/server.mjs';

export { createModelServer, startModelServer } from './orchestration/server.mjs';

// Keep the installed CLI entry stable; orchestration owns server lifecycle.
// 保留安装包的 CLI 入口，服务器生命周期由编排模块负责。
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) startModelServer();
