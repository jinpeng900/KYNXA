import { randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { createInterface } from 'node:readline';
import { createModelServer } from '../../apps/model-gateway/server.mjs';
import { ModelRuntime } from '../../apps/model-gateway/orchestration/runtime.mjs';
import { ModelStore } from '../../apps/model-gateway/models/store.mjs';
import { ConversationStore } from '../../apps/model-gateway/data/conversations.mjs';
import { EmbeddingService } from '../../apps/model-gateway/models/retrieval/embedding-service.mjs';
import { ToolService } from '../../apps/model-gateway/tools/tool-service.mjs';

// The real HTTP gateway uses only this owned temporary catalog; no model credentials or MCP processes.
// 真实 HTTP 网关仅使用独立临时目录；不读取模型凭据，也不启动 MCP 进程。
const temporaryBase = resolve(tmpdir());
const root = await mkdtemp(join(temporaryBase, 'kynxa-retrieval-client-live-'));
const workspace = join(root, 'workspace'), sources = join(root, 'sources');
const dataHome = join(root, 'Data', 'Models'), extensionRoot = join(root, 'Extensions');
await mkdir(workspace, { recursive: true });
await mkdir(sources, { recursive: true });
const globalSource = join(sources, 'global-source.txt'), projectSource = join(sources, 'project-source.txt');
await writeFile(globalSource, 'GLOBAL_RETRIEVAL_FIXTURE 中文资料：全局测试资料。');
await writeFile(projectSource, 'PROJECT_RETRIEVAL_FIXTURE 中文资料：工作测试资料。');
const projectId = randomUUID(), conversationId = randomUUID();
const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
await conversations.saveCatalog({ ...(await conversations.catalog()), Projects: [{ Id: projectId, Name: 'Synthetic retrieval work',
  FolderPath: workspace, Chats: [{ Id: conversationId, Title: 'Synthetic saved chat', Messages: [{ Id: randomUUID(),
    Role: 'user', Content: 'Synthetic message; no real user content.', Status: 'completed' }] }] }], Chats: [] });
const modelStore = new ModelStore({ dataHome });
const tools = new ToolService({ conversationStore: conversations, dataHome, extensionRoot, bundledDirectory: null, officialTools: false });
const runtime = new ModelRuntime({ modelStore, dataHome, extensionRoot, conversationStore: conversations, toolService: tools });
await runtime.retrieval.embeddings.close();
runtime.retrieval.embeddings = new EmbeddingService({ modelRoot: join(root, 'intentionally-no-model') });
const server = createModelServer({ modelStore, modelRuntime: runtime });
await new Promise(resolveReady => server.listen(0, '127.0.0.1', resolveReady));
process.stdout.write(JSON.stringify({ address: `http://127.0.0.1:${server.address().port}`, projectId, conversationId,
  globalSource, projectSource, root }) + '\n');
const input = createInterface({ input: process.stdin });
await new Promise(resolveStop => { input.once('line', resolveStop); input.once('close', resolveStop); });
input.close();
server.closeAllConnections();
await new Promise(resolveClose => server.close(resolveClose));
await server.shutdownModelRuntime();
const suffix = relative(temporaryBase, resolve(root));
if (!suffix || suffix === '..' || suffix.startsWith(`..${sep}`) || resolve(root) !== join(temporaryBase, suffix))
  throw new Error('Refusing cleanup outside the owned temporary fixture.');
await rm(root, { recursive: true, force: true });
