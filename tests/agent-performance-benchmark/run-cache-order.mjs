import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile, mkdir, mkdtemp, rm, stat, realpath } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { performance } from 'node:perf_hooks';
import { CACHE_ORDER_TASKS, cacheOrderPlan, fixtureHash, fixtureId, projectReservations,
  assessNativeTask, summarizeCacheOrder } from './cache-order.mjs';

const failure = code => Object.assign(new Error(code), { code });
const pause = durationMs => new Promise(resolvePause => setTimeout(resolvePause, durationMs));
const fixedCreatedAt = '2026-10-21T00:00:00Z';

/** Refuse changed roots or outside targets before recursive cleanup of this invocation's own data.
 * 递归清理之前核对本次独占根与绝对路径边界；根被替换或目标越界时拒绝删除。
 */
export async function removeOwnedDirectory(ownedRoot, target, canonicalRoot) {
  const rootPath = resolve(ownedRoot), targetPath = resolve(target);
  const outside = child => child === '..' || child.startsWith(`..${sep}`) || isAbsolute(child);
  if (!isAbsolute(ownedRoot) || !isAbsolute(target) || outside(relative(rootPath, targetPath)))
    throw failure('BENCHMARK_CLEANUP_OUTSIDE_OWNED_ROOT');
  if (await realpath(rootPath) !== canonicalRoot) throw failure('BENCHMARK_CLEANUP_ROOT_CHANGED');
  let actualTarget;
  try { actualTarget = await realpath(targetPath); }
  catch (error) { if (error.code === 'ENOENT') return; throw error; }
  if (outside(relative(canonicalRoot, actualTarget))) throw failure('BENCHMARK_CLEANUP_OUTSIDE_OWNED_ROOT');
  await rm(targetPath, { recursive: true, maxRetries: 3, retryDelay: 100 });
}

export function parseOptions(args) {
  const options = { execute: false, repeats: 2, taskTimeoutMs: 300_000 };
  const names = new Map([['--gateway-bin', 'gatewayBin'], ['--application-root', 'applicationRoot'],
    ['--connection-file', 'connectionFile'], ['--model', 'model'], ['--price-file', 'priceFile'],
    ['--output', 'output'], ['--repeats', 'repeats'], ['--task-timeout-ms', 'taskTimeoutMs'], ['--tasks', 'tasks']]);
  for (let index = 0; index < args.length; index++) {
    if (args[index] === '--execute') { options.execute = true; continue; }
    const name = names.get(args[index]);
    if (!name || !args[index + 1] || args[index + 1].startsWith('--')) throw failure('INVALID_CACHE_BENCHMARK_ARGUMENT');
    options[name] = args[++index];
  }
  for (const name of ['repeats', 'taskTimeoutMs']) {
    options[name] = Number(options[name]);
    if (!Number.isSafeInteger(options[name]) || options[name] < 1) throw failure('INVALID_CACHE_BENCHMARK_ARGUMENT');
  }
  options.taskIds = options.tasks ? options.tasks.split(',') : CACHE_ORDER_TASKS.map(task => task.id);
  if (new Set(options.taskIds).size !== options.taskIds.length
    || options.taskIds.some(id => !CACHE_ORDER_TASKS.some(task => task.id === id))) throw failure('INVALID_CACHE_BENCHMARK_TASK');
  if (options.execute && ['gatewayBin', 'applicationRoot', 'connectionFile', 'model'].some(name => !options[name]))
    throw failure('CACHE_BENCHMARK_EXPLICIT_CONFIGURATION_REQUIRED');
  return options;
}

function nativePathTemplate(path) {
  const pathname = new URL(path, 'http://127.0.0.1').pathname;
  if (/^\/api\/conversations\/[^/]+\/messages$/u.test(pathname)) return '/api/conversations/:conversationId/messages';
  if (/^\/api\/tasks\/[^/]+$/u.test(pathname)) return '/api/tasks/:taskId';
  return ['/health', '/api/models', '/api/conversations/catalog', '/api/tasks', '/api/usage/summary',
    '/api/chat/stream', '/api/chat/cancel', '/api/agent/tools'].includes(pathname) ? pathname : 'unknown-native-route';
}

async function nativeHttpFailure(response, method, path) {
  let gatewayCode = null;
  try {
    const payload = await response.json();
    if (typeof payload.code === 'string' && /^[A-Z][A-Z_0-9]{1,99}$/u.test(payload.code)) gatewayCode = payload.code;
  } catch { }
  // Keep route templates and a machine code only; never report query strings, IDs, error prose or bodies.
  // 只保留路由模板与机器错误码；不报告查询参数、身份、错误正文或响应体。
  return Object.assign(failure(`NATIVE_BENCHMARK_HTTP_${response.status}`), { httpDiagnostic: {
    method: ['GET', 'POST', 'PUT', 'DELETE'].includes(method) ? method : 'OTHER',
    pathTemplate: nativePathTemplate(path), status: response.status, gatewayCode } });
}

export async function nativeJson(baseUrl, path, { method = 'GET', body, timeoutMs = 30_000 } = {}) {
  const response = await fetch(new URL(path, baseUrl), { method,
    headers: body === undefined ? {} : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw await nativeHttpFailure(response, method, path);
  return response.json();
}

/** SSE framing follows the existing native API; reading metrics never replays a tool or model call.
 * SSE 使用现有原生合同；读取指标绝不补发模型请求或重做工具操作。 */
export async function sendNativeTurn(baseUrl, input, timeoutMs) {
  const started = performance.now();
  const response = await fetch(new URL('/api/chat/stream', baseUrl), { method: 'POST',
    headers: { 'content-type': 'application/json' }, body: JSON.stringify(input), signal: AbortSignal.timeout(timeoutMs) });
  if (!response.ok) throw await nativeHttpFailure(response, 'POST', '/api/chat/stream');
  if (!response.headers.get('content-type')?.includes('text/event-stream')) throw failure('NATIVE_BENCHMARK_EXPECTED_SSE');
  let pending = '', firstVisibleTextMs = null, terminal = null;
  const decoder = new TextDecoder();
  const consume = () => {
    let boundary;
    while ((boundary = pending.search(/\r?\n\r?\n/u)) >= 0) {
      const frame = pending.slice(0, boundary), delimiter = pending.slice(boundary).match(/^\r?\n\r?\n/u)[0];
      pending = pending.slice(boundary + delimiter.length);
      const data = frame.split(/\r?\n/u).filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n');
      if (!data) continue;
      let event;
      try { event = JSON.parse(data); } catch { throw failure('NATIVE_BENCHMARK_INVALID_SSE'); }
      const visible = event.type === 'text_delta' ? event.delta
        : event.type === 'assistant_segment' ? event.segment?.content : '';
      if (firstVisibleTextMs === null && typeof visible === 'string' && visible.trim()) firstVisibleTextMs = performance.now() - started;
      if (['completed', 'error', 'cancelled'].includes(event.type)) terminal = event;
    }
  };
  for await (const chunk of response.body) {
    pending += decoder.decode(chunk, { stream: true });
    consume();
  }
  pending += decoder.decode();
  consume();
  if (pending.trim() || terminal === null) throw failure('NATIVE_BENCHMARK_INCOMPLETE_SSE');
  return { terminalType: terminal.type,
    terminalCode: typeof terminal.code === 'string' && /^[A-Z][A-Z_0-9]{1,99}$/u.test(terminal.code) ? terminal.code : null,
    firstVisibleTextMs, durationMs: performance.now() - started };
}

async function collectReservations(baseUrl, conversationId) {
  const roots = [];
  let after = '';
  do {
    const page = await nativeJson(baseUrl, `/api/tasks?conversationId=${conversationId}&limit=100${after ? `&afterTaskId=${after}` : ''}`);
    roots.push(...(page.tasks ?? []));
    after = page.hasMore ? page.nextTaskId : '';
    if (page.hasMore && !after) throw failure('NATIVE_BENCHMARK_INVALID_TASK_CURSOR');
  } while (after);
  const pending = roots.map(task => task.taskId), visited = new Set(), reservations = [], states = [];
  while (pending.length) {
    const id = pending.shift();
    if (visited.has(id)) continue;
    visited.add(id);
    const detail = await nativeJson(baseUrl, `/api/tasks/${id}?conversationId=${conversationId}`);
    reservations.push(...(detail.modelReservations ?? []));
    states.push(detail.task.state);
    pending.push(...(detail.children ?? []).map(child => child.taskId));
  }
  return { modelCalls: projectReservations(reservations), taskStates: states };
}

async function appendUserMessage(baseUrl, task, turn, content) {
  const catalog = await nativeJson(baseUrl, '/api/conversations/catalog?includeMessages=true');
  const projectId = fixtureId(`${task.id}:project`), conversationId = fixtureId(`${task.id}:conversation`);
  const chat = catalog.Projects.find(project => project.Id === projectId)?.Chats.find(item => item.Id === conversationId);
  if (!chat) throw failure('NATIVE_BENCHMARK_CHAT_NOT_FOUND');
  chat.Messages ??= [];
  chat.Messages.push({ Id: fixtureId(`${task.id}:user:${turn}`), Role: 'user', Content: content, CreatedAt: fixedCreatedAt });
  await nativeJson(baseUrl, '/api/conversations/catalog', { method: 'PUT', body: catalog });
}

export async function runNativeTask({ baseUrl, task, connection, model, workRoot, taskTimeoutMs = 300_000 }) {
  const conversationId = fixtureId(`${task.id}:conversation`), turns = [];
  const started = performance.now();
  let executionError = null, executionHttpDiagnostic = null;
  try {
    for (const [turn, message] of task.messages.entries()) {
      if (turn > 0) await appendUserMessage(baseUrl, task, turn, message);
      const taskOffsetMs = performance.now() - started;
      turns.push({ taskOffsetMs, ...await sendNativeTurn(baseUrl, { conversationId,
        requestId: fixtureId(`${task.id}:request:${turn}`), userMessageId: fixtureId(`${task.id}:user:${turn}`),
        provider: connection.providerId, model, message, permissionMode: 'smart' }, taskTimeoutMs) });
      if (turns.at(-1).terminalType !== 'completed') break;
    }
  } catch (error) {
    executionError = safeErrorCode(error);
    executionHttpDiagnostic = error.httpDiagnostic ?? null;
    // A benchmark observation deadline stops only its owned request, before its synthetic workspace is reused.
    // 评测观察截止只取消本评测自有请求；其合成工作目录复用前必须停止该请求，不能后台重复写入。
    const requestId = fixtureId(`${task.id}:request:${turns.length}`);
    try { await nativeJson(baseUrl, '/api/chat/cancel', { method: 'POST', body: { conversationId, requestId } }); } catch { }
  }
  const durationMs = performance.now() - started;
  // Fetch formal messages even after a failed stream; the already saved answer and receipts remain authoritative.
  // 即便流读取失败，也读取正式消息；已有答复与回执仍以正式存储为准，不能丢弃失败调用成本。
  const messages = await nativeJson(baseUrl, `/api/conversations/${conversationId}/messages?limit=200`);
  const assistants = (messages.messages ?? []).filter(item => item.Role === 'assistant');
  const disk = {};
  for (const title of new Set([...Object.keys(task.files), ...(task.effect ? [task.effect.target] : [])])) {
    try { disk[title] = await readFile(join(workRoot, task.id, title), 'utf8'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  const journal = await collectReservations(baseUrl, conversationId);
  const usageSummary = await nativeJson(baseUrl, `/api/usage/summary?conversationId=${conversationId}`);
  const assessment = assessNativeTask(task, assistants, disk);
  assessment.checks.nativeTaskCompleted = journal.taskStates.length > 0 && journal.taskStates.every(state => state === 'completed');
  assessment.success = Object.values(assessment.checks).every(Boolean) && executionError === null;
  const firstVisibleTurn = turns.find(turn => turn.firstVisibleTextMs !== null);
  return { taskId: task.id, ...assessment, executionError, executionHttpDiagnostic, turns, durationMs, setupMs: 0,
    ...journal, usageSummary, taskFirstVisibleTextMs: firstVisibleTurn
      ? firstVisibleTurn.taskOffsetMs + firstVisibleTurn.firstVisibleTextMs : null,
    // Save verifier results, never upstream secrets, full prompts, machine-specific paths or raw reservation documents.
    // 只保存评分与计量，不保存上游密钥、完整提示、机器私有路径或原始预约文档。
    answers: assistants.map(item => ({ status: item.Status, contentHash: fixtureHash(item.Content) })) };
}

async function freePort() {
  const server = createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const port = server.address().port;
  await new Promise(resolveClose => server.close(resolveClose));
  return port;
}

function isolatedEnvironment(root, applicationRoot, port, mode) {
  const environment = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (/^(?:PATH|SystemRoot|windir|COMSPEC|TEMP|TMP|USERPROFILE|APPDATA|LOCALAPPDATA|PROGRAMDATA|NUMBER_OF_PROCESSORS|PROCESSOR_.*|HTTPS?_PROXY|ALL_PROXY|NO_PROXY|SSL_CERT_FILE|SSL_CERT_DIR)$/iu.test(name))
      environment[name] = value;
  }
  return { ...environment, KYNXA_APPLICATION_ROOT: applicationRoot, KYNXA_DATA_HOME: join(root, 'Data'),
    KYNXA_MODEL_HOME: join(root, 'Data', 'Models'), KYNXA_EXTENSION_HOME: join(root, 'Extensions'),
    KYNXA_STORAGE_POINTER: join(root, 'unused-storage.json'), KYNXA_EXTENSION_POINTER: join(root, 'unused-extensions.json'),
    KYNXA_MODEL_API_PORT: String(port), KYNXA_BENCHMARK_TOOL_ORDER: mode,
    PATH: `${join(applicationRoot, 'runtime', 'native')}${process.platform === 'win32' ? ';' : ':'}${environment.PATH ?? environment.Path ?? ''}` };
}

async function stopOwnedGateway(child) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') {
    const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await once(killer, 'exit');
  } else child.kill('SIGTERM');
  if (child.exitCode === null && child.signalCode === null) await once(child, 'exit');
}

/** Startup diagnostics expose machine codes and exit facts only, never captured stderr or private paths.
 * 启动诊断只公开机器错误码和退出事实，不公开捕获的标准错误正文、私有路径或凭据。
 */
export function startupDiagnostic({ exitCode = null, signal = null, stderr = '', spawnErrorCode = null } = {}) {
  const codes = new Set();
  for (const line of stderr.split(/\r?\n/u)) {
    let code = line.match(/^([A-Z][A-Z_0-9]{1,99}):/u)?.[1];
    try { code ??= JSON.parse(line).code; } catch { }
    if (typeof code === 'string' && /^[A-Z][A-Z_0-9]{1,99}$/u.test(code)) codes.add(code);
    if (codes.size >= 8) break;
  }
  return { exitCode, exitCodeHex: Number.isInteger(exitCode) ? `0x${(exitCode >>> 0).toString(16).padStart(8, '0')}` : null,
    signal: typeof signal === 'string' && /^SIG[A-Z0-9]+$/u.test(signal) ? signal : null,
    spawnErrorCode: typeof spawnErrorCode === 'string' && /^[A-Z_0-9]{1,100}$/u.test(spawnErrorCode) ? spawnErrorCode : null,
    gatewayCodes: [...codes], capturedStderrBytes: Buffer.byteLength(stderr) };
}

async function startOwnedGateway(options, root, mode) {
  await mkdir(join(root, 'Data', 'Models'), { recursive: true });
  await mkdir(join(root, 'Extensions'), { recursive: true });
  const port = await freePort(), baseUrl = `http://127.0.0.1:${port}`;
  const child = spawn(resolve(options.gatewayBin), [], { cwd: resolve(options.applicationRoot), windowsHide: true,
    env: isolatedEnvironment(root, resolve(options.applicationRoot), port, mode), stdio: ['ignore', 'ignore', 'pipe'] });
  let launchError = null, capturedStderr = Buffer.alloc(0), stderrTruncated = false;
  child.on('error', error => { launchError = error.code ?? 'UNKNOWN'; });
  child.stderr.on('data', chunk => {
    const available = Math.max(0, 16_384 - capturedStderr.length);
    if (chunk.length > available) stderrTruncated = true;
    capturedStderr = Buffer.concat([capturedStderr, chunk.subarray(0, available)]);
  });
  const diagnose = () => ({ ...startupDiagnostic({ exitCode: child.exitCode, signal: child.signalCode,
    spawnErrorCode: launchError, stderr: capturedStderr.toString('utf8') }), stderrTruncated });
  try {
    const deadline = performance.now() + 60_000;
    while (performance.now() < deadline) {
      if (launchError || child.exitCode !== null || child.signalCode !== null) throw failure('NATIVE_BENCHMARK_GATEWAY_EXITED');
      let health;
      try { health = await nativeJson(baseUrl, '/health', { timeoutMs: 1000 }); } catch { await pause(200); continue; }
      if (health.runtime !== 'rust' || health.benchmarkToolOrder !== mode) throw failure('NATIVE_BENCHMARK_ORDERING_UNSUPPORTED');
      return { baseUrl, close: () => stopOwnedGateway(child) };
    }
    throw failure('NATIVE_BENCHMARK_GATEWAY_START_TIMEOUT');
  } catch (error) {
    error.startupDiagnostic = diagnose();
    await stopOwnedGateway(child);
    throw error;
  }
}

async function createArmFixture(baseUrl, tasks, workRoot, connection, model) {
  await nativeJson(baseUrl, '/api/models', { method: 'POST', body: { ...connection, models: [model] } });
  const catalog = await nativeJson(baseUrl, '/api/conversations/catalog?includeMessages=false');
  const projects = [];
  for (const task of tasks) {
    const folder = join(workRoot, task.id);
    await mkdir(folder, { recursive: true });
    for (const [title, content] of Object.entries(task.files)) await writeFile(join(folder, title), content, 'utf8');
    projects.push({ Id: fixtureId(`${task.id}:project`), Name: `Cache order ${task.id}`, FolderPath: folder,
      Chats: [{ Id: fixtureId(`${task.id}:conversation`), Title: task.id, Draft: '',
        Messages: [{ Id: fixtureId(`${task.id}:user:0`), Role: 'user', Content: task.messages[0], CreatedAt: fixedCreatedAt }] }] });
  }
  const saved = await nativeJson(baseUrl, '/api/conversations/catalog', { method: 'PUT', body: { Revision: catalog.Revision, Chats: [], Projects: projects } });
  for (const task of tasks) {
    const project = saved.Projects?.find(item => item.Id === fixtureId(`${task.id}:project`));
    if (!project?.Chats?.some(chat => chat.Id === fixtureId(`${task.id}:conversation`)))
      throw failure('NATIVE_BENCHMARK_CATALOG_IDENTITY_MISMATCH');
  }
  const catalogTools = await nativeJson(baseUrl, '/api/agent/tools');
  if (!Array.isArray(catalogTools.tools) || catalogTools.tools.length === 0) throw failure('BENCHMARK_TOOL_CATALOG_MISSING');
  return fixtureHash((catalogTools.tools ?? []).map(tool => ({ name: tool.name,
    description: tool.description, inputSchema: tool.inputSchema ?? tool.parameters })).sort((left, right) => left.name.localeCompare(right.name)));
}

async function loadConnection(path, model) {
  const input = JSON.parse(await readFile(path, 'utf8'));
  const endpoint = new URL(input.baseUrl);
  if (!['http:', 'https:'].includes(endpoint.protocol) || endpoint.username || endpoint.password || endpoint.search || endpoint.hash)
    throw failure('INVALID_BENCHMARK_ENDPOINT');
  if (!['openai-completions', 'openai-responses', 'anthropic-messages', 'ollama'].includes(input.protocol)
    || typeof input.providerId !== 'string' || !input.providerId.trim()) throw failure('INVALID_BENCHMARK_CONNECTION');
  const apiKey = input.apiKeyEnv ? process.env[input.apiKeyEnv] : input.apiKey ?? '';
  if (input.apiKeyEnv && !apiKey) throw failure('BENCHMARK_API_KEY_ENV_MISSING');
  return { providerId: input.providerId, displayName: 'Isolated cache ordering benchmark', baseUrl: input.baseUrl,
    protocol: input.protocol, apiKey, models: [model], contextWindowTokens: input.contextWindowTokens ?? 32768,
    maxOutputTokens: input.maxOutputTokens ?? 4096 };
}

export function safeErrorCode(error) {
  return typeof error?.code === 'string' && /^[A-Z][A-Z_0-9]{0,100}$/u.test(error.code)
    ? error.code : error?.name === 'TimeoutError' ? 'NATIVE_BENCHMARK_OBSERVATION_TIMEOUT' : 'NATIVE_BENCHMARK_FAILED';
}

export async function runCacheOrder(options) {
  const plan = cacheOrderPlan(options);
  if (!options.execute) return plan;
  await stat(resolve(options.gatewayBin));
  // serve() requires this sidecar even when the fixture needs no ToolHost or embedding model.
  // 即使夹具不需要 ToolHost 或检索模型，serve() 仍要求此原生资源服务；使用只读 bundle 根。
  try {
    if (!(await stat(join(resolve(options.applicationRoot), 'runtime', 'resource', 'kynxa-resource-service.exe'))).isFile())
      throw failure('NATIVE_BENCHMARK_RESOURCE_RUNTIME_MISSING');
  } catch { throw failure('NATIVE_BENCHMARK_RESOURCE_RUNTIME_MISSING'); }
  const connection = await loadConnection(options.connectionFile, options.model);
  const price = options.priceFile ? JSON.parse(await readFile(options.priceFile, 'utf8')) : null;
  if (price && (price.model !== options.model || price.provider !== connection.providerId)) throw failure('BENCHMARK_PRICE_IDENTITY_MISMATCH');
  // Validate rates before a real request, then use them only for transparent token-cost estimates.
  // 在真实请求之前验证价格表；其后只计算透明的 token 成本估计，不推测账单。
  summarizeCacheOrder([], price);
  const tasks = CACHE_ORDER_TASKS.filter(task => options.taskIds.includes(task.id));
  const root = await mkdtemp(join(tmpdir(), 'kynxa-cache-order-'));
  const canonicalRoot = await realpath(root);
  const workRoot = join(root, 'Work'), runs = [], armMetadata = [];
  let benchmarkError = null, benchmarkStartupDiagnostic = null, benchmarkHttpDiagnostic = null;
  try {
    if (options.output) await writeFile(resolve(options.output), JSON.stringify({ ...plan,
      measurementKind: 'native-model-tool-order-ab-pending', runs: [] }, null, 2), { flag: 'wx' });
    try {
      for (const [index, sequence] of plan.sequence.entries()) {
        for (const arm of sequence) {
          // The same synthetic workspace path and IDs remove irrelevant prefix changes; only its owned test data is reset.
          // 两臂复用相同的合成工作区路径与身份，减少无关前缀变化；只重置本评测独占的数据。
          await removeOwnedDirectory(root, workRoot, canonicalRoot);
          const armRoot = join(root, `repeat-${index + 1}-${arm}`), setup = performance.now();
          const gateway = await startOwnedGateway(options, armRoot, plan.arms[arm]);
          try {
            const toolCatalogHash = await createArmFixture(gateway.baseUrl, tasks, workRoot, connection, options.model);
            if (armMetadata.some(item => item.toolCatalogHash !== toolCatalogHash)) throw failure('BENCHMARK_TOOL_CATALOG_MISMATCH');
            armMetadata.push({ repetition: index + 1, arm, toolOrder: plan.arms[arm], toolCatalogHash,
              setupMs: performance.now() - setup });
            for (const task of tasks) {
              const taskStarted = performance.now();
              try {
                runs.push({ repetition: index + 1, arm,
                  ...await runNativeTask({ baseUrl: gateway.baseUrl, task, connection, model: options.model, workRoot,
                    taskTimeoutMs: options.taskTimeoutMs }) });
              } catch (error) {
                runs.push({ repetition: index + 1, arm, taskId: task.id, success: false,
                  checks: { formalRecordsAvailable: false }, executionError: safeErrorCode(error),
                  httpDiagnostic: error.httpDiagnostic ?? null,
                  unmeasuredCalls: true, modelCalls: [], durationMs: performance.now() - taskStarted, setupMs: 0 });
                throw error;
              }
              if (options.output) await writeFile(resolve(options.output), JSON.stringify({ ...plan,
                measurementKind: 'native-model-tool-order-ab-pending', runs }, null, 2));
              if (runs.at(-1).executionError) throw failure('NATIVE_BENCHMARK_TASK_OBSERVATION_FAILED');
            }
          } finally { await gateway.close(); }
        }
      }
    } catch (error) {
      benchmarkError = safeErrorCode(error);
      benchmarkStartupDiagnostic = error.startupDiagnostic ?? null;
      benchmarkHttpDiagnostic = error.httpDiagnostic ?? null;
    }
    const report = { ...plan, measurementKind: benchmarkError ? 'native-model-tool-order-ab-incomplete' : 'native-model-tool-order-ab',
      benchmarkError, benchmarkStartupDiagnostic, benchmarkHttpDiagnostic,
      realCalls: runs.some(run => run.unmeasuredCalls) ? null : runs.reduce((sum, run) => sum + run.modelCalls.length, 0),
      provider: connection.providerId, model: options.model, protocol: connection.protocol,
      contextWindowTokens: connection.contextWindowTokens, maxOutputTokens: connection.maxOutputTokens,
      taskObservationTimeoutMs: options.taskTimeoutMs, armsRun: armMetadata, runs,
      summary: summarizeCacheOrder(runs, price), armsSummary: Object.fromEntries(['A', 'B'].map(arm =>
        [arm, summarizeCacheOrder(runs.filter(run => run.arm === arm), price)])),
      limitations: ['No forced tool-category call; tool selection, task state, recovery and compression use native production flow.',
        'Remote cache residency and provider sampling are not controlled; alternating arms does not guarantee a cache reset.',
        'Tool-selection accuracy uses fixed fixture allow-lists, not a universal judge; no statistical quality claim from this small suite.',
        'Observation timeouts apply only to this isolated experiment and never change production long-task budgets.'] };
    if (options.output) await writeFile(resolve(options.output), JSON.stringify(report, null, 2));
    return report;
  } finally {
    // Only the unique directory allocated above is removed after owned gateway processes have exited.
    // 只在自有网关进程退出后删除上面独占创建的临时目录，不清理其他测试或正式数据。
    await removeOwnedDirectory(root, root, canonicalRoot);
  }
}

if (import.meta.url === pathToFileURL(resolve(process.argv[1] ?? '')).href) {
  try {
    const report = await runCacheOrder(parseOptions(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    if (report.benchmarkError) process.exitCode = 1;
  }
  catch (error) { process.stderr.write(`${safeErrorCode(error)}\n`); process.exitCode = 1; }
}
