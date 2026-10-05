import { createHash, randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ConversationStore } from '../../apps/model-gateway/data/conversations.mjs';
import { MemoryService } from '../../apps/model-gateway/data/memory-service.mjs';
import { validateConnection } from '../../apps/model-gateway/models/store.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../../apps/model-gateway/models/retrieval/embedding-profile.mjs';
import { ModelRuntime } from '../../apps/model-gateway/orchestration/runtime.mjs';
import { ToolService } from '../../apps/model-gateway/tools/tool-service.mjs';

import { FIXTURE_VERSION, VERIFIER_VERSION, RUN_LIMITS, SOURCE_TITLE, SOURCE_TEXT, RECEIPT_TEXT, TOOL_NAMES, TASKS,
  answerLanguageDiagnostic, benchmarkPlan, readUsage, verifyTask, summarizeRuns } from './evaluation.mjs';
import { readSourceFingerprint } from './source-fingerprint.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const ARTIFACT_ROOT = join(REPOSITORY_ROOT, 'artifacts', 'verification', 'agent-performance-benchmark');
const FIXTURE_ROOT = join(ARTIFACT_ROOT, 'fixtures');
const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const within = (root, path) => { const suffix = relative(resolve(root), resolve(path));
  return suffix === '' || !isAbsolute(suffix) && suffix !== '..' && !suffix.startsWith(`..${sep}`); };
const safeCode = error => /^[A-Z0-9_]{1,100}$/.test(error?.code ?? '') ? error.code : 'BENCHMARK_FAILED';
const failure = code => Object.assign(new Error(code), { code });

/**
 * The connection stays in memory; the production connection validator defines its contract.
 * 连接仅保存在内存中，格式由生产连接校验器定义，不保存 Models/connections.json。
 */
export async function loadBenchmarkConnection(path, { provider, model } = {}) {
  try {
    const text = await readFile(path, 'utf8');
    if (Buffer.byteLength(text, 'utf8') > 2 * 1024 * 1024) throw failure('INVALID_BENCHMARK_CONNECTION');
    const document = JSON.parse(text.replace(/^\uFEFF/, ''));
    const entries = document.version === 1 && Array.isArray(document.providers) ? document.providers : [document];
    const selected = provider ? entries.find(item => item.providerId === provider) : entries.length === 1 ? entries[0] : null;
    if (!selected) throw failure('BENCHMARK_PROVIDER_REQUIRED');
    const connection = validateConnection({ ...selected, maxOutputTokens: 8192, contextWindowTokens: 32768 });
    const selectedModel = model ?? (connection.models.length === 1 ? connection.models[0] : null);
    if (!selectedModel || !connection.models.includes(selectedModel)) throw failure('BENCHMARK_MODEL_REQUIRED');
    return { connection: Object.freeze({ ...connection, models: Object.freeze([...connection.models]) }), model: selectedModel };
  } catch (error) { throw failure(safeCode(error) === 'BENCHMARK_FAILED' ? 'INVALID_BENCHMARK_CONNECTION' : safeCode(error)); }
}

function redact(value, connection) {
  let serialized = JSON.stringify(value);
  for (const secret of [connection.apiKey, connection.baseUrl]) if (secret)
    serialized = serialized.split(JSON.stringify(secret).slice(1, -1)).join('[redacted]');
  return JSON.parse(serialized);
}

/**
 * This adapter limits fixture authority; selection, tool execution, caching and history stay production code.
 * 适配器只收紧测试资料的权限，选择、执行、缓存和历史仍复用生产代码；不启动终端、浏览器、MCP 或外网工具。
 */
export class FixtureToolService extends ToolService {
  async catalog(context, options) {
    const descriptors = await super.catalog(context, options);
    if (context) {
      const snapshot = this.catalogs.get(context);
      snapshot.descriptors = new Map([...snapshot.descriptors].filter(([name]) => TOOL_NAMES.has(name)));
    }
    return descriptors.filter(tool => TOOL_NAMES.has(tool.name));
  }

  async execute(context, call, options) {
    const path = call.arguments?.path ?? '.';
    if (!TOOL_NAMES.has(call.name) || call.name.startsWith('filesystem.') &&
      (typeof path !== 'string' || !within(context.workspaceRoot, resolve(context.workspaceRoot, path))))
      return { isError: true, status: 'error', code: 'FIXTURE_ACCESS_DENIED', content: '{"ok":false,"code":"FIXTURE_ACCESS_DENIED"}' };
    return super.execute(context, call, options);
  }
}

async function cleanupFixture(root) {
  if (!within(FIXTURE_ROOT, root) || !basename(root).startsWith('kynxa-agent-benchmark-')) throw failure('UNSAFE_FIXTURE_CLEANUP');
  await rm(root, { recursive: true, force: true });
}

async function runTask(connection, model, task, { rag, semantic, measurementKind, repetition }) {
  const setupStarted = performance.now();
  // Use an owned evaluation root so personal home-directory names are not sent as tool paths.
  // 使用评测自有目录，避免将个人用户目录名作为工具路径发送给测试模型。
  await mkdir(FIXTURE_ROOT, { recursive: true });
  const root = await mkdtemp(join(FIXTURE_ROOT, 'kynxa-agent-benchmark-'));
  let runtime, timer;
  const originalFetch = globalThis.fetch, modelCalls = [], retrievalCalls = [], assistants = [];
  let setupMs = 0, durationMs = 0, started, errorCode, closeErrorCode, cleanupErrorCode, sourceId, finalFile;
  try {
    const workspace = join(root, 'Work'), dataHome = join(root, 'Data', 'Models'), extensionRoot = join(root, 'Extensions');
    await mkdir(workspace); await mkdir(extensionRoot);
    await writeFile(join(workspace, SOURCE_TITLE), SOURCE_TEXT, 'utf8');
    await writeFile(join(workspace, 'seed.txt'), RECEIPT_TEXT, 'utf8');
    const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null }), conversationId = randomUUID(), projectId = randomUUID();
    await conversations.saveCatalog({ Revision: (await conversations.catalog()).Revision,
      Projects: [{ Id: projectId, Name: 'Synthetic agent fixture', FolderPath: workspace,
        Chats: [{ Id: conversationId, Title: task.id, Messages: [{ Id: randomUUID(), Role: 'user', Status: 'completed',
          Content: 'Synthetic benchmark initial state.' }] }] }], Chats: [] });
    const memory = new MemoryService({ conversationStore: conversations });
    const tools = new FixtureToolService({ conversationStore: conversations, dataHome, extensionRoot,
      extensionPointer: join(root, 'unused-extension-pointer.json'), officialTools: false, bundledDirectory: null });
    runtime = new ModelRuntime({ modelStore: { connectionFor: async provider => provider === connection.providerId ? connection : null },
      dataHome, extensionRoot, conversationStore: conversations, memoryService: memory, toolService: tools,
      timeoutMs: RUN_LIMITS.maxDurationMs, streamTimeoutMs: RUN_LIMITS.maxDurationMs });
    await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0,
      patch: { local: { enabled: rag, semantic: rag ? semantic : 'off', rerankProfileId: null }, web: { mode: 'off', browserRead: 'off' } } });
    if (task.id !== 'greeting') {
      if (rag && semantic === 'auto' && runtime.retrieval.embeddings.status().state !== 'ready') throw failure('BENCHMARK_EMBEDDING_UNAVAILABLE');
      const imported = await runtime.retrieval.importSource({ path: join(workspace, SOURCE_TITLE), scope: 'user' });
      sourceId = imported.id;
      await runtime.retrieval.activeJobs.get(imported.jobId)?.promise;
      if ((await runtime.retrieval.jobs.get(imported.jobId)).status !== 'completed') throw failure('BENCHMARK_FIXTURE_INDEX_FAILED');
      if (rag && semantic === 'auto' && !(await runtime.retrieval.index.status()).vectorChunks) throw failure('BENCHMARK_FIXTURE_VECTOR_MISSING');
    }
    // Observe production retrieval outcomes without replacing retrieval or its query routing.
    // 只观察生产检索结果，不替代检索算法或查询路由。
    const search = runtime.retrieval.search.bind(runtime.retrieval);
    runtime.retrieval.search = async (...args) => {
      const before = performance.now(), result = await search(...args);
      retrievalCalls.push({ durationMs: Math.ceil(performance.now() - before), strategy: result.strategy,
        vectorAvailable: result.vectorAvailable, sourceTitles: result.items.map(item => item.title),
        evidenceAssessment: result.evidenceAssessment });
      return result;
    };
    setupMs = Math.ceil(performance.now() - setupStarted); started = performance.now();
    const deadline = started + RUN_LIMITS.maxDurationMs;
    timer = setTimeout(() => runtime.shutdown.abort(), RUN_LIMITS.maxDurationMs);
    // The standalone process meters every provider request, including errors, and denies all other network destinations.
    // 独立进程计量全部供应商调用（包括失败），拒绝其余网络目的地；凭据及私有 URL 不进入记录。
    globalThis.fetch = async (url, options = {}) => {
      if (![`${connection.baseUrl}/chat/completions`, `${connection.baseUrl}/responses`, `${connection.baseUrl}/messages`].includes(String(url)))
        throw failure('BENCHMARK_NETWORK_DENIED');
      if (modelCalls.length >= RUN_LIMITS.maxRounds) throw failure('BENCHMARK_MODEL_CALL_LIMIT');
      const remainingMs = Math.floor(deadline - performance.now());
      if (remainingMs <= 0) throw failure('BENCHMARK_TASK_TIMEOUT');
      const call = { number: modelCalls.length + 1, durationMs: 0, status: 'error', usage: readUsage(connection.protocol, null) };
      modelCalls.push(call); const before = performance.now();
      try {
        const response = await originalFetch(url, { ...options, signal: AbortSignal.any([
          ...(options.signal ? [options.signal] : []), AbortSignal.timeout(remainingMs)]) });
        call.httpStatus = response.status;
        const payload = await response.clone().json().catch(() => null);
        call.usage = readUsage(connection.protocol, payload); call.status = response.ok ? 'completed' : 'http-error';
        if (payload !== null) {
          const safePayload = redact(payload, connection), headers = new Headers(response.headers);
          headers.delete('content-length'); headers.delete('content-encoding');
          await response.body?.cancel();
          return new Response(JSON.stringify(safePayload), { status: response.status, headers });
        }
        return response;
      } finally { call.durationMs = Math.ceil(performance.now() - before); }
    };
    for (const message of task.messages) {
      if (modelCalls.length >= RUN_LIMITS.maxRounds) throw failure('BENCHMARK_MODEL_CALL_LIMIT');
      const previousToolCalls = assistants.reduce((sum, assistant) => sum + (assistant.ToolActivities?.length ?? 0), 0);
      if (previousToolCalls >= RUN_LIMITS.maxToolCalls) throw failure('BENCHMARK_TOOL_CALL_LIMIT');
      const requestId = randomUUID();
      try {
        await runtime.reply({ conversationId, requestId, userMessageId: randomUUID(), provider: connection.providerId, model, message,
          permissionMode: 'full', runLimits: { ...RUN_LIMITS, maxRounds: RUN_LIMITS.maxRounds - modelCalls.length,
            maxToolCalls: RUN_LIMITS.maxToolCalls - previousToolCalls,
            maxDurationMs: Math.max(1000, Math.floor(deadline - performance.now())) } });
      } finally {
        const assistant = (await conversations.readModelMessages(conversationId)).find(item => item.Id === requestId);
        if (assistant) assistants.push(assistant);
      }
    }
    finalFile = await readFile(join(workspace, 'receipt.txt'), 'utf8').catch(() => undefined);
  } catch (error) { errorCode = safeCode(error); }
  finally {
    clearTimeout(timer); globalThis.fetch = originalFetch;
    if (!setupMs) setupMs = Math.ceil(performance.now() - setupStarted);
    durationMs = started === undefined ? 0 : Math.ceil(performance.now() - started);
    // A failed native shutdown is not proof that its files are idle; keep the fixture and stop this batch.
    // 原生关闭失败不能证明文件已空闲；保留资料供检查，并停止本批后续任务，不递归删除未排空的目录。
    try { await runtime?.close(); }
    catch (error) { closeErrorCode = safeCode(error); }
    if (!closeErrorCode) {
      try { await cleanupFixture(root); }
      catch (error) { cleanupErrorCode = safeCode(error); }
    }
  }
  const verification = verifyTask(task.id, { assistants, sourceId, finalFile });
  const tools = assistants.flatMap(item => item.ToolActivities ?? []);
  return redact({ taskId: task.id, configuration: rag ? 'rag-on' : 'rag-off', semantic: rag ? semantic : 'off', measurementKind, repetition,
    executionStatus: closeErrorCode ? 'close-error' : cleanupErrorCode ? 'cleanup-error'
      : errorCode ? started === undefined ? 'setup-error' : 'error' : 'completed',
    setupMs, durationMs, success: !errorCode && !closeErrorCode && !cleanupErrorCode && verification.success, checks: verification.checks,
    ...(closeErrorCode ? { closeErrorCode } : {}), ...(cleanupErrorCode ? { cleanupErrorCode } : {}),
    ...(closeErrorCode || cleanupErrorCode ? { retainedFixturePath: root } : {}),
    ...(errorCode ? { errorCode } : {}), modelCalls, retrievalCalls,
    ...(task.id === 'dependent-file-cycle' ? { languageDiagnostic: answerLanguageDiagnostic(assistants.at(-1)?.Content) } : {}),
    toolCalls: tools.length, executedToolCalls: tools.filter(item => !item.reused && item.code !== 'MODEL_TOOL_UNAVAILABLE').length,
    reusedToolCalls: tools.filter(item => item.reused).length,
    failedToolCalls: tools.filter(item => item.status !== 'completed').length,
    toolRounds: new Set(tools.map(item => `${assistants.findIndex(assistant => assistant.ToolActivities?.includes(item))}:${item.round}`)).size,
    answers: assistants.map(item => ({ content: item.Content, status: item.Status,
      evidenceTitles: (item.EvidenceReferences ?? []).map(reference => reference.title) })),
    toolTrace: tools.map(item => ({ name: item.name, status: item.status, round: item.round,
      ...(item.reused ? { reused: true } : {}), ...(item.code ? { code: item.code } : {}) })) }, connection);
}

export async function runBenchmark({ connection, model, rag = 'paired', semantic = 'auto',
  repeats = 1, measurementKind = 'agent-task-evaluation', onProgress = () => {} }) {
  if (!['off', 'on', 'paired'].includes(rag) || !['auto', 'off'].includes(semantic)) throw failure('INVALID_BENCHMARK_OPTION');
  if (!['agent-task-evaluation', 'functional-regression'].includes(measurementKind)) throw failure('INVALID_BENCHMARK_OPTION');
  if (!Number.isSafeInteger(repeats) || repeats < 1 || repeats > 2) throw failure('INVALID_BENCHMARK_OPTION');
  const validated = validateConnection({ ...connection, contextWindowTokens: 32768, maxOutputTokens: 8192 });
  connection = Object.freeze({ ...validated, models: Object.freeze([...validated.models]) });
  if (!connection.models.includes(model)) throw failure('BENCHMARK_MODEL_REQUIRED');
  const configurations = rag === 'paired' ? [false, true] : [rag === 'on'];
  const sourceFingerprint = await readSourceFingerprint();
  const config = { model, protocol: connection.protocol, contextWindowTokens: 32768, maxOutputTokens: 8192, repeats,
    runLimits: RUN_LIMITS, tools: [...TOOL_NAMES].sort(), semantic, rerank: null, web: 'off', fixtureVersion: FIXTURE_VERSION,
    embeddingVersion: semantic === 'auto' ? BUILTIN_EMBEDDING_PROFILE.modelVersion : null,
    providerSampling: 'production protocol defaults; no harness temperature or seed override' };
  const runs = [], planned = Array.from({ length: repeats }, (_, index) => index + 1).flatMap(repetition =>
    TASKS.flatMap(task => (repetition % 2 === 1 ? configurations : [...configurations].reverse()).map(enabled =>
      ({ taskId: task.id, configuration: enabled ? 'rag-on' : 'rag-off', repetition }))));
  let globalBlocked;
  tasksLoop: for (const plan of planned) {
    const task = TASKS.find(item => item.id === plan.taskId), enabled = plan.configuration === 'rag-on';
    onProgress({ ...plan, state: 'starting' });
    const run = await runTask(connection, model, task, { rag: enabled, semantic, measurementKind, repetition: plan.repetition });
    runs.push(run); onProgress({ ...plan, state: 'completed', success: run.success });
    if (run.closeErrorCode) {
      globalBlocked = { code: 'BENCHMARK_CLOSE_FAILED', afterTaskId: run.taskId, configuration: run.configuration,
        repetition: run.repetition, retainedFixturePath: run.retainedFixturePath };
      break tasksLoop;
    }
  }
  return { schemaVersion: 1, createdAt: new Date().toISOString(), fixtureVersion: FIXTURE_VERSION, fixtureHash: hash([SOURCE_TEXT, RECEIPT_TEXT, TASKS]),
    measurementKind, verifierVersion: VERIFIER_VERSION, sourceFingerprint,
    model, protocol: connection.protocol, configHash: hash([config, hash(connection.baseUrl)]),
    config, plannedRunCount: planned.length, unexecutedRuns: planned.slice(runs.length), ...(globalBlocked ? { globalBlocked } : {}),
    runs, summary: summarizeRuns(runs), configurations: Object.fromEntries(configurations.map(enabled => {
      const name = enabled ? 'rag-on' : 'rag-off'; return [name, summarizeRuns(runs.filter(run => run.configuration === name))]; })),
    repetitions: Object.fromEntries(Array.from({ length: repeats }, (_, index) => {
      const repetition = index + 1, subset = runs.filter(run => run.repetition === repetition);
      return [repetition, { summary: summarizeRuns(subset), configurations: Object.fromEntries(configurations.map(enabled => {
        const name = enabled ? 'rag-on' : 'rag-off'; return [name, summarizeRuns(subset.filter(run => run.configuration === name))]; })) }];
    })) };
}

async function main(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const name = args[index];
    if (name === '--functional-regression') { options.functionalRegression = true; continue; }
    if (!['--connection-file', '--provider', '--model', '--rag', '--semantic', '--output', '--repeats'].includes(name) || !args[index + 1] || args[index + 1].startsWith('--'))
      throw failure('INVALID_BENCHMARK_ARGUMENT');
    if (Object.hasOwn(options, name.slice(2))) throw failure('INVALID_BENCHMARK_ARGUMENT');
    options[name.slice(2)] = args[++index];
  }
  if (!options['connection-file']) { process.stdout.write(JSON.stringify(benchmarkPlan(), null, 2) + '\n'); return; }
  const { connection, model } = await loadBenchmarkConnection(options['connection-file'], options);
  if (options.functionalRegression && !['127.0.0.1', 'localhost', '[::1]'].includes(new URL(connection.baseUrl).hostname))
    throw failure('FUNCTIONAL_REGRESSION_REQUIRES_LOOPBACK');
  const output = resolve(options.output ?? join(ARTIFACT_ROOT, `run-${Date.now()}-${randomUUID().slice(0, 8)}`));
  if (!within(ARTIFACT_ROOT, output) || output === resolve(ARTIFACT_ROOT)) throw failure('INVALID_BENCHMARK_OUTPUT');
  await mkdir(ARTIFACT_ROOT, { recursive: true });
  try { await mkdir(output); }
  catch (error) { throw failure(error.code === 'EEXIST' ? 'BENCHMARK_OUTPUT_EXISTS' : 'BENCHMARK_OUTPUT_UNAVAILABLE'); }
  const result = await runBenchmark({ connection, model, rag: options.rag ?? 'paired', semantic: options.semantic ?? 'auto',
    repeats: options.repeats === undefined ? 1 : Number(options.repeats),
    measurementKind: options.functionalRegression ? 'functional-regression' : 'agent-task-evaluation',
    onProgress: progress => process.stderr.write(JSON.stringify(progress) + '\n') });
  await writeFile(join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n', { encoding: 'utf8', flag: 'wx' });
  process.stdout.write(JSON.stringify({ measurementKind: result.measurementKind, model, protocol: connection.protocol,
    configHash: result.configHash, resultsPath: join(output, 'results.json'), configurations: result.configurations,
    ...(result.globalBlocked ? { globalBlocked: result.globalBlocked } : {}) }, null, 2) + '\n');
  if (result.globalBlocked) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  main(process.argv.slice(2)).catch(error => { process.stderr.write(JSON.stringify({ errorCode: safeCode(error) }) + '\n'); process.exitCode = 1; });
