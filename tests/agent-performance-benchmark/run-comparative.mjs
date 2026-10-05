import { createHash, randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { ConversationStore } from '../../apps/model-gateway/data/conversations.mjs';
import { MemoryService } from '../../apps/model-gateway/data/memory-service.mjs';
import { ModelRuntime } from '../../apps/model-gateway/orchestration/runtime.mjs';
import { BUILTIN_EMBEDDING_PROFILE } from '../../apps/model-gateway/models/retrieval/embedding-profile.mjs';
import { estimateTokens } from '../../apps/model-gateway/models/context-tokens.mjs';
import { validateConnection } from '../../apps/model-gateway/models/store.mjs';
import { FixtureToolService, loadBenchmarkConnection } from './run-agent.mjs';
import { readUsage, summarizeRuns, TOOL_NAMES } from './evaluation.mjs';
import { readSourceFingerprint } from './source-fingerprint.mjs';
import { createPiReference, PI_REFERENCE_SDK_ROOT } from './pi-reference.mjs';
import { SYSTEM_SUITE_VERSION, SYSTEM_VERIFIER_VERSION, SYSTEM_LIMITS, SYSTEM_TASKS,
  SYSTEM_SUPPORT_FINGERPRINT_INPUT, verifySystemTask, summarizeSystemChecks } from './system-suite.mjs';

const ROOT = fileURLToPath(new URL('../../artifacts/verification/agent-performance-benchmark/', import.meta.url));
const FIXTURES = join(ROOT, 'system-fixtures');
const ENGINES = ['kynxa', 'pi-sdk'];
const failure = code => Object.assign(new Error(code), { code });
const safeCode = error => /^[A-Z0-9_]{1,100}$/u.test(error?.code ?? '') ? error.code : 'BENCHMARK_FAILED';
const sha = value => createHash('sha256').update(typeof value === 'string' || Buffer.isBuffer(value) ? value : JSON.stringify(value)).digest('hex');
const within = (root, target) => { const suffix = relative(resolve(root), resolve(target));
  return suffix === '' || !isAbsolute(suffix) && suffix !== '..' && !suffix.startsWith(`..${sep}`); };
const redact = (value, connection) => {
  let text = JSON.stringify(value);
  for (const secret of [connection.apiKey, connection.baseUrl]) if (secret)
    text = text.split(JSON.stringify(secret).slice(1, -1)).join('[redacted]');
  return JSON.parse(text);
};
const percentile = (values, fraction) => values.length ? [...values].sort((a, b) => a - b)[Math.max(0, Math.ceil(values.length * fraction) - 1)] : null;

/** Count transport attempts and SSE usage without logging provider headers, credentials or request bodies.
 * 计量真实传输尝试与流式用量，不记录供应商请求头、凭据或原始请求体；两个执行器使用相同观测边界。 */
export function installProviderMeter(connection, { deadline, signal, maxCalls = SYSTEM_LIMITS.maxRounds, taskStarted = performance.now() }) {
  const original = globalThis.fetch, calls = [], drains = [];
  const meterStarted = taskStarted;
  const endpoint = connection.baseUrl.replace(/\/$/u, '') + '/chat/completions';
  globalThis.fetch = async (input, options = {}) => {
    const url = input instanceof Request ? input.url : String(input);
    if (url !== endpoint) throw failure('BENCHMARK_NETWORK_DENIED');
    if (calls.length >= maxCalls) throw failure('BENCHMARK_MODEL_CALL_LIMIT');
    const remaining = Math.floor(deadline - performance.now());
    if (remaining <= 0) throw failure('BENCHMARK_TASK_TIMEOUT');
    const started = performance.now(), call = { number: calls.length + 1, status: 'error', durationMs: 0,
      startedOffsetMs: Math.ceil(started - meterStarted), firstTextMs: null, usage: readUsage(connection.protocol, null) };
    calls.push(call);
    let body;
    try { body = JSON.parse(options.body); } catch { throw failure('BENCHMARK_INVALID_PROVIDER_REQUEST'); }
    // Include usage on both SSE transports; this changes metering only, not prompts, sampling or tools.
    // 两条流式链路都请求用量回执，只改变计量字段，不改变提示、采样或工具内容。
    if (body.stream) body.stream_options = { ...body.stream_options, include_usage: true };
    const messages = JSON.stringify(body.messages ?? []), system = (body.messages ?? []).filter(item => item.role === 'system');
    call.evidenceObservations = system.flatMap(item => String(item.content ?? '').split('\n').flatMap(line => {
      try { const record = JSON.parse(line); return typeof record.title === 'string' && typeof record.excerpt === 'string' &&
        /^ev1:|^rag1:/u.test(record.sourceRef ?? '') ? [{ title: record.title, text: record.excerpt }] : []; } catch { return []; }
    }));
    const refLengths = [...messages.matchAll(/ev1:[A-Za-z0-9_-]{22}:[0-9a-z]{2}|rag1:[A-Za-z0-9_-]+/gu)].map(item => item[0].length);
    const ids = new Set((body.messages ?? []).flatMap(item => item.tool_calls ?? []).map(item => item.id));
    const resultIds = new Set((body.messages ?? []).filter(item => item.role === 'tool').map(item => item.tool_call_id));
    call.request = { stream: body.stream === true, toolSchemas: body.tools?.length ?? 0, maxOutputTokens: body.max_tokens ?? body.max_completion_tokens,
      estimatedMessageTokens: estimateTokens(messages), estimatedSchemaTokens: estimateTokens(JSON.stringify(body.tools ?? [])),
      messageCount: body.messages?.length ?? 0, systemMessages: system.length,
      evidenceReferences: refLengths.length, referenceCharacterLengths: [...new Set(refLengths)].sort((a, b) => a - b),
      orphanToolResults: (body.messages ?? []).filter(item => item.role === 'tool' && !ids.has(item.tool_call_id)).length,
      missingToolResults: [...ids].filter(id => !resultIds.has(id)).length };
    try {
      const response = await original(input, { ...options, body: JSON.stringify(body), signal: AbortSignal.any([
        ...(options.signal ? [options.signal] : []), ...(signal ? [signal] : []), AbortSignal.timeout(remaining)]) });
      call.httpStatus = response.status;
      const observe = async () => {
        try {
          if (!(response.headers.get('content-type') ?? '').includes('text/event-stream')) {
            const payload = await response.clone().json();
            call.usage = readUsage(connection.protocol, payload);
            call.status = response.ok ? 'completed' : 'http-error';
          } else {
            const reader = response.clone().body.getReader(), decoder = new TextDecoder();
            let buffered = '', done = false;
            while (!done) {
              const next = await reader.read(); done = next.done; buffered += decoder.decode(next.value, { stream: !done });
              const lines = buffered.split('\n'); buffered = lines.pop();
              for (const line of lines) {
                if (!line.startsWith('data:')) continue;
                const data = line.slice(5).trim();
                if (data === '[DONE]') { call.status = response.ok ? 'completed' : 'http-error'; continue; }
                let chunk; try { chunk = JSON.parse(data); } catch { continue; }
                if (chunk.usage) call.usage = readUsage(connection.protocol, chunk);
                const choice = chunk.choices?.[0];
                if (choice?.delta?.content && call.firstTextMs === null) call.firstTextMs = Math.ceil(performance.now() - started);
                if (choice?.finish_reason) call.finishReason = choice.finish_reason;
              }
            }
            if (call.status === 'error' && response.ok) call.status = 'stream-without-done';
          }
        } catch { call.status = 'observation-error'; }
        finally { call.durationMs = Math.ceil(performance.now() - started); }
      };
      drains.push(observe());
      return response;
    } catch (error) { call.errorCode = safeCode(error); call.durationMs = Math.ceil(performance.now() - started); throw error; }
  };
  return { calls, async close() { globalThis.fetch = original; await Promise.allSettled(drains); } };
}

async function cleanup(root) {
  if (!within(FIXTURES, root) || !basename(root).startsWith('agent-system-')) throw failure('UNSAFE_FIXTURE_CLEANUP');
  await rm(root, { recursive: true, force: true });
}

async function runOne({ connection, model, task, engine, repetition, semantic, sdkRoot, measurementKind }) {
  const setupStarted = performance.now();
  await mkdir(FIXTURES, { recursive: true });
  const root = await mkdtemp(join(FIXTURES, 'agent-system-'));
  let runtime, reference, meter, timer, started, setupMs, durationMs, errorCode, closeErrorCode, cleanupErrorCode, currentTurn, workspace;
  const assistants = [], retrievalCalls = [], disk = {}, signal = new AbortController();
  let referenceSnapshot;
  try {
    workspace = join(root, 'Work');
    const dataHome = join(root, 'Data', 'Models'), extensionRoot = join(root, 'Extensions');
    await mkdir(workspace); await mkdir(extensionRoot);
    for (const [name, content] of Object.entries(task.files)) await writeFile(join(workspace, name), content, 'utf8');
    const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null }), conversationId = randomUUID(), projectId = randomUUID();
    await conversations.saveCatalog({ Revision: (await conversations.catalog()).Revision,
      Projects: [{ Id: projectId, Name: 'Synthetic system benchmark', FolderPath: workspace,
        Chats: [{ Id: conversationId, Title: task.id, Messages: [{ Id: randomUUID(), Role: 'user', Status: 'completed',
          Content: 'Synthetic benchmark initial state.' }] }] }], Chats: [] });
    const memory = new MemoryService({ conversationStore: conversations });
    const tools = new FixtureToolService({ conversationStore: conversations, dataHome, extensionRoot,
      extensionPointer: join(root, 'unused-extension-pointer.json'), officialTools: false, bundledDirectory: null });
    runtime = new ModelRuntime({ modelStore: { connectionFor: async provider => provider === connection.providerId ? connection : null },
      dataHome, extensionRoot, conversationStore: conversations, memoryService: memory, toolService: tools,
      timeoutMs: SYSTEM_LIMITS.maxDurationMs, streamTimeoutMs: SYSTEM_LIMITS.maxDurationMs });
    await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0,
      patch: { local: { enabled: true, semantic, rerankProfileId: null }, web: { mode: 'off', browserRead: 'off' } } });
    if (!task.greeting && task.index !== false) {
      if (semantic === 'auto' && runtime.retrieval.embeddings.status().state !== 'ready') throw failure('BENCHMARK_EMBEDDING_UNAVAILABLE');
      for (const name of Object.keys(task.files)) {
        const imported = await runtime.retrieval.importSource({ path: join(workspace, name), scope: 'user' });
        await runtime.retrieval.activeJobs.get(imported.jobId)?.promise;
        if ((await runtime.retrieval.jobs.get(imported.jobId)).status !== 'completed') throw failure('BENCHMARK_FIXTURE_INDEX_FAILED');
      }
      if (semantic === 'auto' && !(await runtime.retrieval.index.status()).vectorChunks) throw failure('BENCHMARK_FIXTURE_VECTOR_MISSING');
    }
    const search = runtime.retrieval.search.bind(runtime.retrieval);
    runtime.retrieval.search = async (...args) => {
      const before = performance.now(), result = await search(...args);
      retrievalCalls.push({ durationMs: Math.ceil(performance.now() - before), strategy: result.strategy,
        titles: result.items.map(item => item.title), evidenceAssessment: result.evidenceAssessment });
      return result;
    };
    setupMs = Math.ceil(performance.now() - setupStarted); started = performance.now();
    const deadline = started + SYSTEM_LIMITS.maxDurationMs;
    timer = setTimeout(() => { signal.abort(); runtime.shutdown.abort(); reference?.abort(); }, SYSTEM_LIMITS.maxDurationMs);
    meter = installProviderMeter(connection, { deadline, signal: signal.signal, taskStarted: started });
    for (const message of task.messages) {
      const firstCall = meter.calls.length;
      const previousToolCalls = assistants.reduce((sum, item) => sum + (item.ToolActivities?.length ?? 0), 0);
      if (firstCall >= SYSTEM_LIMITS.maxRounds) throw failure('BENCHMARK_MODEL_CALL_LIMIT');
      if (previousToolCalls >= SYSTEM_LIMITS.maxToolCalls) throw failure('BENCHMARK_TOOL_CALL_LIMIT');
      const requestId = randomUUID(), input = { conversationId, requestId, userMessageId: randomUUID(), provider: connection.providerId, model,
        message, permissionMode: 'full', runLimits: { ...SYSTEM_LIMITS, maxRounds: Math.max(1, SYSTEM_LIMITS.maxRounds - meter.calls.length),
          maxToolCalls: SYSTEM_LIMITS.maxToolCalls - previousToolCalls,
          maxDurationMs: Math.max(1000, Math.floor(deadline - performance.now())) } };
      let turn;
      try {
        if (engine === 'kynxa') {
          await runtime.replyStream(input, () => {}, signal.signal);
        } else {
          turn = await runtime.prepare(input, conversationId);
          currentTurn = turn;
          await conversations.upsertMessage(conversationId, turn.assistant);
          if (!reference) reference = await createPiReference({ sdkRoot, connection, model, service: tools, catalog: turn.catalog,
            systemPrompt: turn.requestOptions.system, initialMessages: turn.messages.slice(0, -1), limits: SYSTEM_LIMITS,
            maxOutputTokens: turn.requestOptions.maxOutputTokens, inputBudgetTokens: turn.inputBudgetTokens, signal: signal.signal,
            onToolActivity: async (activity, context) => {
              if (context.requestId !== currentTurn.assistant.Id) throw failure('BENCHMARK_RECEIPT_OWNER_MISMATCH');
              await runtime.saveToolActivity(conversationId, currentTurn, activity);
            } });
          // Each prompt uses fresh formal receipt ownership while Pi retains its own native multi-turn history.
          // 每个用户消息使用新的正式回执归属，Pi 仍保留自身原生多轮历史，不由宿主伪造循环。
          const result = await reference.prompt(message, { context: turn.toolContext ?? { conversationId, requestId },
            systemPrompt: turn.requestOptions.system });
          turn.assistant = { ...turn.assistant, ...result.assistant, Role: 'assistant' };
          await conversations.upsertMessage(conversationId, turn.assistant);
          if (result.errorCode) throw failure(result.errorCode);
        }
      } finally {
        const assistant = (await conversations.readModelMessages(conversationId)).find(item => item.Id === requestId);
        if (assistant) assistants.push({ ...assistant, BenchmarkObservations: meter.calls.slice(firstCall).flatMap(call => call.evidenceObservations) });
        if (turn?.toolContext) await tools.releaseContext(turn.toolContext);
      }
    }
  } catch (error) { errorCode = safeCode(error); }
  finally {
    clearTimeout(timer); if (meter) await meter.close();
    durationMs = started === undefined ? 0 : Math.ceil(performance.now() - started);
    setupMs ??= Math.ceil(performance.now() - setupStarted);
    if (workspace && task.effect) disk[task.effect.target] = await readFile(join(workspace, task.effect.target), 'utf8').catch(() => undefined);
    referenceSnapshot = reference?.snapshot();
    try { await reference?.close(); } catch (error) { closeErrorCode = safeCode(error); }
    try { await runtime?.close(); } catch (error) { closeErrorCode ??= safeCode(error); }
    if (!closeErrorCode) { try { await cleanup(root); } catch (error) { cleanupErrorCode = safeCode(error); } }
  }
  const verification = verifySystemTask(task, { assistants, disk }), activities = assistants.flatMap(item => item.ToolActivities ?? []);
  const calls = meter?.calls ?? [];
  for (const [index, call] of calls.entries()) {
    const native = referenceSnapshot?.modelCalls.filter(item => item.attempted)[index];
    if (call.usage.status !== 'reported' && native?.usage.status === 'reported') { call.usage = native.usage; call.usageSource = 'native-sdk'; }
  }
  return redact({ taskId: task.id, category: task.category, engine, configuration: engine, repetition, measurementKind,
    executionStatus: errorCode || closeErrorCode || cleanupErrorCode ? 'error' : 'completed',
    success: !errorCode && !closeErrorCode && !cleanupErrorCode && verification.success, checks: verification.checks,
    setupMs, durationMs, modelCalls: calls, retrievalCalls, toolCalls: activities.length,
    failedToolCalls: activities.filter(item => item.status !== 'completed').length,
    finishedToolActivities: activities.filter(item => item.status !== 'running').length,
    firstTextMs: calls.find(item => item.firstTextMs !== null)
      ? calls.find(item => item.firstTextMs !== null).startedOffsetMs + calls.find(item => item.firstTextMs !== null).firstTextMs : null,
    ...(errorCode ? { errorCode } : {}), ...(closeErrorCode ? { closeErrorCode, retainedFixturePath: root } : {}),
    ...(cleanupErrorCode ? { cleanupErrorCode, retainedFixturePath: root } : {}),
    ...(referenceSnapshot ? { reference: referenceSnapshot.reference } : {}),
    answers: assistants.map(item => ({ content: item.Content, status: item.Status,
      evidenceTitles: (item.EvidenceReferences ?? []).map(reference => reference.title),
      retrievalResultRef: item.RetrievalResultRef ?? null })),
    toolTrace: activities.map(item => ({ name: item.name, arguments: item.arguments, status: item.status, round: item.round,
      result: item.result, resultRef: item.resultRef ?? null, ...(item.reused ? { reused: true } : {}), ...(item.code ? { code: item.code } : {}) })),
    disk }, connection);
}

function engineSummary(runs) {
  return { ...summarizeRuns(runs), checks: summarizeSystemChecks(runs),
    latency: { taskP50Ms: percentile(runs.map(run => run.durationMs), .5), taskP95Ms: percentile(runs.map(run => run.durationMs), .95),
      firstTextP50Ms: percentile(runs.map(run => run.firstTextMs).filter(Number.isFinite), .5),
      firstTextP95Ms: percentile(runs.map(run => run.firstTextMs).filter(Number.isFinite), .95) },
    categories: Object.fromEntries([...new Set(runs.map(run => run.category))].map(category => [category, summarizeRuns(runs.filter(run => run.category === category))])) };
}

/** Paired engines share fixed model, tools, source text, input/output and execution limits.
 * 两个执行器配对使用固定模型、工具、资料和预算；次轮反转顺序，原始结果逐条追加后再汇总。 */
export async function runComparative({ connection, model, sdkRoot = PI_REFERENCE_SDK_ROOT, semantic = 'auto', repeats = 2,
  taskIds, measurementKind = 'agent-task-evaluation', onProgress = () => {}, onRun = async () => {}, onPlan = async () => {} } = {}) {
  if (!['auto', 'off'].includes(semantic) || ![1, 2].includes(repeats) || connection.protocol !== 'openai-completions')
    throw failure('INVALID_BENCHMARK_OPTION');
  const tasks = taskIds ? SYSTEM_TASKS.filter(task => taskIds.includes(task.id)) : SYSTEM_TASKS;
  if (!tasks.length || taskIds?.some(id => !tasks.some(task => task.id === id))) throw failure('INVALID_BENCHMARK_TASK');
  connection = Object.freeze(validateConnection({ ...connection, contextWindowTokens: 32768, maxOutputTokens: 8192 }));
  if (!connection.models.includes(model)) throw failure('BENCHMARK_MODEL_REQUIRED');
  const baseFingerprint = await readSourceFingerprint();
  const own = await Promise.all(['system-suite.mjs', 'run-comparative.mjs', 'pi-reference.mjs', 'system-suite.test.mjs',
    'comparative.test.mjs', 'pi-reference.test.mjs'].map(async name => ({
    path: `tests/agent-performance-benchmark/${name}`, sha256: sha(await readFile(new URL(name, import.meta.url))) })));
  const sourceFingerprint = { algorithm: 'sha256', files: [...baseFingerprint.files, ...own], combinedHash: sha([...baseFingerprint.files, ...own]) };
  // Validate and preload the optional actual peer before any paid call; a missing peer must not produce half a comparison.
  // 在任何付费调用前校验并预加载可选真实对照，缺失依赖不能产生只有一半的比较，也不将首次导入成本记在 Pi 请求里。
  const readiness = await createPiReference({ sdkRoot, connection, model, service: { execute: async () => { throw failure('BENCHMARK_PREFLIGHT_TOOL_FORBIDDEN'); } },
    catalog: [], systemPrompt: '', limits: SYSTEM_LIMITS, onToolActivity: async () => { throw failure('BENCHMARK_PREFLIGHT_TOOL_FORBIDDEN'); } });
  const peerReference = readiness.reference;
  await readiness.close();
  const config = { model, protocol: connection.protocol, tools: [...TOOL_NAMES].sort(), contextWindowTokens: 32768,
    maxOutputTokens: 8192, limits: SYSTEM_LIMITS, semantic, embeddingVersion: semantic === 'auto' ? BUILTIN_EMBEDDING_PROFILE.modelVersion : null,
    rerank: null, web: 'off', engines: ENGINES, repeats, sharedPreparation: 'KYNXA RAG + system prompt + initial tool catalog',
    piScope: 'actual SDK core, not Pi CLI/product', providerSampling: 'native protocol defaults; no temperature or seed override',
    streamUsage: 'include_usage enabled for both transports', fixtureVersion: SYSTEM_SUITE_VERSION, verifierVersion: SYSTEM_VERIFIER_VERSION };
  const planned = Array.from({ length: repeats }, (_, index) => index + 1).flatMap(repetition => tasks.flatMap(task =>
    (repetition % 2 ? ENGINES : [...ENGINES].reverse()).map(engine => ({ taskId: task.id, engine, repetition }))));
  const manifest = { fixtureHash: sha([tasks, SYSTEM_SUPPORT_FINGERPRINT_INPUT]), configHash: sha([config, peerReference, sha(connection.baseUrl)]),
    config, peerReference, sourceFingerprint, planned };
  await onPlan(manifest);
  const runs = [];
  for (const plan of planned) {
    onProgress({ ...plan, state: 'starting' });
    const run = await runOne({ ...plan, task: tasks.find(task => task.id === plan.taskId), connection, model, sdkRoot, semantic, measurementKind });
    runs.push(run); await onRun(run); onProgress({ ...plan, state: 'completed', success: run.success,
      ...(run.errorCode ? { errorCode: run.errorCode } : {}) });
    if (run.closeErrorCode) break;
  }
  return { schemaVersion: 2, createdAt: new Date().toISOString(), measurementKind, fixtureVersion: SYSTEM_SUITE_VERSION,
    verifierVersion: SYSTEM_VERIFIER_VERSION, fixtureHash: manifest.fixtureHash, configHash: manifest.configHash,
    config, peerReference, sourceFingerprint, plannedRunCount: planned.length, unexecutedRuns: planned.slice(runs.length), runs,
    engines: Object.fromEntries(ENGINES.map(engine => [engine, engineSummary(runs.filter(run => run.engine === engine))])) };
}

async function main(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const name = args[index];
    if (!['--connection-file', '--provider', '--model', '--semantic', '--repeats', '--output', '--tasks'].includes(name) || !args[index + 1]) throw failure('INVALID_BENCHMARK_ARGUMENT');
    if (Object.hasOwn(options, name)) throw failure('INVALID_BENCHMARK_ARGUMENT'); options[name] = args[++index];
  }
  if (!options['--connection-file']) {
    process.stdout.write(JSON.stringify({ fixtureVersion: SYSTEM_SUITE_VERSION, realCalls: 0, tasks: SYSTEM_TASKS.map(task => ({ id: task.id, category: task.category })),
      limits: SYSTEM_LIMITS, reference: '@earendil-works/pi-agent-core 1.0.3', notCovered: ['desktop UI', 'browser', 'remote MCP', '200-message recovery'] }, null, 2) + '\n'); return;
  }
  const { connection, model } = await loadBenchmarkConnection(options['--connection-file'], { provider: options['--provider'], model: options['--model'] });
  const output = resolve(options['--output'] ?? join(ROOT, `system-v2-${Date.now()}-${randomUUID().slice(0, 8)}`));
  if (!within(ROOT, output) || output === resolve(ROOT)) throw failure('INVALID_BENCHMARK_OUTPUT');
  await mkdir(ROOT, { recursive: true });
  try { await mkdir(output); } catch (error) { throw failure(error.code === 'EEXIST' ? 'BENCHMARK_OUTPUT_EXISTS' : 'BENCHMARK_OUTPUT_UNAVAILABLE'); }
  const log = join(output, 'runs.jsonl');
  await writeFile(log, '', { flag: 'wx' });
  const { appendFile } = await import('node:fs/promises');
  const result = await runComparative({ connection, model, semantic: options['--semantic'] ?? 'auto', repeats: Number(options['--repeats'] ?? 2),
    taskIds: options['--tasks']?.split(','), onProgress: progress => process.stderr.write(JSON.stringify(progress) + '\n'),
    onPlan: manifest => writeFile(join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx' }),
    onRun: run => appendFile(log, JSON.stringify(run) + '\n', 'utf8') });
  await writeFile(join(output, 'results.json'), JSON.stringify(result, null, 2) + '\n', { encoding: 'utf8', flag: 'wx' });
  process.stdout.write(JSON.stringify({ resultsPath: join(output, 'results.json'), fixtureVersion: result.fixtureVersion,
    configHash: result.configHash, engines: result.engines, unexecutedRuns: result.unexecutedRuns }, null, 2) + '\n');
  if (result.unexecutedRuns.length) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  main(process.argv.slice(2)).catch(error => { process.stderr.write(JSON.stringify({ errorCode: safeCode(error) }) + '\n'); process.exitCode = 1; });
