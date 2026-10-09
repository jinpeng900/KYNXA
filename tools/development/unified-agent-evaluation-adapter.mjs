import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { ModelRuntime } from '../../apps/model-gateway/orchestration/runtime.mjs';
import { ConversationStore } from '../../apps/model-gateway/data/conversations.mjs';
import { ToolService } from '../../apps/model-gateway/tools/tool-service.mjs';
import { ResourceBudgetService } from '../../apps/model-gateway/platform/resources/resource-client.mjs';
import { runResourceTask } from '../../apps/model-gateway/platform/resources/resource-task.mjs';
import { inspectLocalPath, within } from '../../apps/model-gateway/platform/tool-paths.mjs';
import { isLocalEndpoint, validateConnection } from '../../apps/model-gateway/models/store.mjs';
import { MAX_QUERY_CHARACTERS } from '../../apps/model-gateway/data/retrieval/retrieval-contracts.mjs';
import { authorization, chatRequest, protocols } from '../../apps/model-gateway/models/protocols.mjs';
import { RETRIEVAL_ABLATIONS } from './evaluate-unified-retrieval.mjs';

const MAX_TRANSFER_BYTES = 16 * 1024 * 1024;
const MAX_FIXTURE_BYTES = 512 * 1024 * 1024;
const MAX_FIXTURE_FILES = 20000;
const FIXED_BUDGET = Object.freeze({ channelCandidates: 48, fusedCandidates: 64, limit: 8,
  maximumTokens: 8192, source: 'evaluation-fixed' });
const CHECK_DESCRIPTOR = Object.freeze({ name: 'terminal.run', source: 'builtin', enabled: true,
  description: 'Run one declared Node test in the copied workspace: command must be node, args must be ["--test", "check-ID"]. No other command, path or argument is allowed.',
  inputSchema: { type: 'object', properties: { command: { type: 'string', enum: ['node'] },
    args: { type: 'array', items: { type: 'string' }, minItems: 2, maxItems: 2 } }, required: ['command', 'args'], additionalProperties: false } });
const BASE_TOOLS = new Set(['filesystem.list', 'filesystem.read', 'filesystem.stat', 'filesystem.search',
  'filesystem.write', 'filesystem.edit', 'filesystem.delete', 'filesystem.mkdir', 'knowledge.search', 'knowledge.read',
  'tool.search', 'tool.load', 'tool.result.read']);
const failure = (code, message) => Object.assign(new Error(message), { code });
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const knownNumber = value => Number.isSafeInteger(value) && value >= 0 ? value : null;

function inside(root, path) {
  const target = resolve(root, path);
  if (!within(root, target)) throw failure('EVALUATION_SCOPE_DENIED', 'Path escapes the copied workspace. / 路径超出评测副本。');
  return target;
}

/** Copy regular files only, with hard limits; an agent never receives the original fixture or validator directory.
 * 只复制有界普通文件；模型不能访问原始样本或独立验收目录。 */
async function copyFixture(source, destination) {
  await inspectLocalPath(source);
  source = await realpath(source);
  const files = [], pending = [{ source, destination }];
  let bytes = 0, entries = 0;
  while (pending.length) {
    const directory = pending.pop();
    await inspectLocalPath(directory.source);
    await mkdir(directory.destination, { recursive: true });
    for (const entry of (await readdir(directory.source, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      if (++entries > 200000) throw failure('EVALUATION_FIXTURE_LIMIT', 'Fixture traversal exceeds the directory-entry limit.');
      const path = join(directory.source, entry.name), target = join(directory.destination, entry.name), info = await lstat(path);
      if (info.isSymbolicLink() || !info.isDirectory() && !info.isFile() || info.isFile() && info.nlink !== 1)
        throw failure('EVALUATION_UNSAFE_FIXTURE', 'Linked/special fixture paths are unsupported. / 样本不允许链接或特殊文件。');
      if (info.isDirectory()) pending.push({ source: path, destination: target });
      else {
        if (files.length >= MAX_FIXTURE_FILES || info.size > MAX_TRANSFER_BYTES || (bytes += info.size) > MAX_FIXTURE_BYTES)
          throw failure('EVALUATION_FIXTURE_LIMIT', 'Fixture exceeds explicit copy limits. / 样本超过明确复制预算。');
        const content = await readFile(path);
        if (content.length !== info.size) throw failure('EVALUATION_FIXTURE_CHANGED', 'Fixture changed while copying. / 复制时样本已变化。');
        await writeFile(target, content, { flag: 'wx' });
        files.push({ path, copiedPath: target, sha256: digest(content) });
      }
    }
  }
  return files;
}

async function verifyFiles(files) {
  for (const file of files) {
    await inspectLocalPath(file.path);
    if (digest(await readFile(file.path)) !== file.sha256)
      throw failure('EVALUATION_FIXTURE_CHANGED', 'Original fixture/independent validator changed. / 原始样本或独立验收器发生变化。');
  }
}

/** Fixed-resource variants still use the same physical authority; they disable tuning, rather than inventing a second capacity.
 * 固定资源组仍由同一物理资源服务审批，只关闭调优，不另造容量或放松安全上限。 */
export function evaluationResourceView(service, adaptive) {
  if (adaptive) return service;
  const fixedGrant = grant => ({ ...grant, suggestions: { ...grant.suggestions, ...FIXED_BUDGET,
    candidateLimit: FIXED_BUDGET.channelCandidates, fusedCandidateLimit: FIXED_BUDGET.fusedCandidates,
    evidenceBudgetTokens: FIXED_BUDGET.maximumTokens, batchMultiplier: 1, annBuildConcurrency: 1,
    source: 'evaluation-fixed' } });
  return {
    async acquire(options, control) {
      if (options.gpuMemoryBytes > 0) return { status: 'denied', reason: 'EVALUATION_CPU_ONLY', code: 'EVALUATION_CPU_ONLY', mode: 'evaluation-fixed' };
      return fixedGrant(await service.acquire({ ...options, cpuThreads: Math.min(2, options.cpuThreads ?? 1), gpuMemoryBytes: 0 }, control));
    },
    renew: (...args) => service.renew(...args), release: (...args) => service.release(...args),
    registerExecutor: (...args) => service.registerExecutor(...args), snapshot: (...args) => service.snapshot(...args),
    report: async () => ({ state: 'disabled', reason: 'evaluation-fixed' }),
    status: (...args) => service.status(...args)
  };
}

function validateVariant(variant) {
  const supported = RETRIEVAL_ABLATIONS.find(item => item.id === variant?.id);
  if (!supported || ['gaps', 'adaptiveResources', 'relations', 'experience'].some(key => supported[key] !== variant[key]))
    throw failure('EVALUATION_VARIANT_UNSUPPORTED', 'Unsupported or relabeled ablation. / 消融能力组合无效，不能仅改标签。');
  return supported;
}

function validateCheck(check) {
  if (!check || typeof check.id !== 'string' || !check.id || typeof check.script !== 'string' ||
      isAbsolute(check.script) || !/\.(?:mjs|cjs|js)$/u.test(check.script) || check.script.split(/[\\/]/u).includes('..') ||
      !Array.isArray(check.args ?? []) || (check.args ?? []).length !== 0 ||
      !Number.isSafeInteger(check.timeoutMs ?? 10000) || (check.timeoutMs ?? 10000) < 100 || (check.timeoutMs ?? 10000) > 120000)
    throw failure('EVALUATION_CHECK_INVALID', 'Only declared Node tests without arbitrary arguments are supported. / 只支持声明的 Node 测试，不支持任意参数。');
  return { ...check, args: check.args ?? [], timeoutMs: check.timeoutMs ?? 10000 };
}

function childEnvironment(workspace, validators) {
  // Never give a generated fixture the model API key or arbitrary inherited user credentials.
  // 不把模型 API 密钥或任意继承的用户凭据传给生成代码。
  const environment = {};
  for (const key of ['SystemRoot', 'WINDIR', 'PATH', 'PATHEXT', 'COMSPEC'])
    if (process.env[key] !== undefined) environment[key] = process.env[key];
  return { ...environment, TEMP: workspace, TMP: workspace, HOME: workspace, USERPROFILE: workspace,
    KYNXA_EVALUATION_WORKSPACE: workspace, KYNXA_EVALUATION_VALIDATORS: validators };
}

function measuredAttempt(requests) {
  const tokens = field => requests.length && requests.every(request => request[field] !== null)
    ? requests.reduce((sum, request) => sum + request[field], 0) : null;
  // A multi-round agent run is one task attempt; ordinary tool feedback is not a network retry.
  // 多轮智能体执行仍是一次任务尝试，正常工具反馈不能计作网络重试。
  return { modelCalls: requests.length, toolCalls: requests.reduce((sum, request) => sum + request.toolCalls, 0),
    inputTokens: tokens('inputTokens'), outputTokens: tokens('outputTokens') };
}

async function executeCheck(check, workspace, validators, resources, originalValidators, signal) {
  const script = inside(validators, check.script);
  await inspectLocalPath(script);
  await verifyFiles(originalValidators.map(file => ({ ...file, path: file.copiedPath })));
  return runResourceTask(resources, { taskId: `evaluation-check:${randomUUID()}`, kind: 'foreground',
    cpuThreads: 1, memoryBytes: 64 * 1024 * 1024 }, lease => new Promise((resolveResult, reject) => {
    const args = ['--permission', `--allow-fs-read=${workspace}`, `--allow-fs-read=${validators}`,
      '--test', '--test-isolation=none', '--test-reporter=tap', check.script];
    const child = spawn(process.execPath, args, { cwd: validators, windowsHide: true, env: childEnvironment(workspace, validators),
      stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '', outputBytes = 0, limited = false, timedOut = false;
    const append = chunk => {
      outputBytes += chunk.length;
      if (outputBytes > 65536) { limited = true; child.kill(); }
      else output += chunk.toString('utf8');
    };
    child.stdout.on('data', append); child.stderr.on('data', append);
    const timer = setTimeout(() => { timedOut = true; child.kill(); }, check.timeoutMs);
    const abort = () => child.kill(); signal?.addEventListener('abort', abort, { once: true });
    // The lease stays reserved until close; registration measures only our own validator child.
    // 租约保留到进程关闭，注册仅测量本次拥有的验收子进程。
    if (child.pid && lease?.leaseId) resources.registerExecutor?.(lease.leaseId, { processId: child.pid }).catch(() => {});
    child.once('error', error => { clearTimeout(timer); signal?.removeEventListener('abort', abort); reject(error); });
    child.once('close', (exitCode, exitSignal) => {
      clearTimeout(timer); signal?.removeEventListener('abort', abort);
      const summary = name => { const value = new RegExp(`^# ${name} (\\d+)$`, 'mu').exec(output); return value ? Number(value[1]) : null; };
      const completedTests = summary('tests'), passedTests = summary('pass'), failedTests = summary('fail'), cancelledTests = summary('cancelled');
      const hasTestReceipt = completedTests !== null && completedTests > 0 && passedTests === completedTests &&
        failedTests === 0 && cancelledTests === 0;
      resolveResult({ id: check.id, label: check.label ?? check.id, exitCode,
        passed: exitCode === 0 && hasTestReceipt && !timedOut && !limited && !signal?.aborted,
        completedTests, passedTests, timedOut, outputLimited: limited,
        exitSignal, output, validatorSha256: originalValidators.find(file => file.copiedPath === script)?.sha256 ?? null });
    });
  }), { signal });
}

async function readBounded(stream) {
  const chunks = []; let bytes = 0;
  for await (const chunk of stream) {
    if ((bytes += chunk.length) > MAX_TRANSFER_BYTES) throw failure('EVALUATION_API_SIZE', 'Model payload exceeds the evaluation limit.');
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks);
}

/** A local bounded transport measures actual provider usage without patching global fetch or persisting credentials.
 * 本地有界转发测量供应商实际 usage，不改全局 fetch，也不把凭据落盘。 */
async function measuredTransport(connection, model, sampling, attempts) {
  const expectedPath = chatRequest(connection, model, []).path;
  const transportKey = randomUUID();
  const server = createServer(async (request, response) => {
    const presentedKey = connection.protocol === 'anthropic-messages' ? request.headers['x-api-key'] :
      request.headers.authorization?.replace(/^Bearer /u, '');
    if (presentedKey !== transportKey) { response.writeHead(403); response.end(); return; }
    const attempt = { modelCalls: 1, toolCalls: 0, inputTokens: null, outputTokens: null, durationMs: 0 };
    const started = performance.now(); attempts.push(attempt);
    try {
      if (request.method !== 'POST' || request.url !== `/v1${expectedPath}`) throw failure('EVALUATION_API_ROUTE', 'Unexpected model route.');
      const body = JSON.parse((await readBounded(request)).toString('utf8'));
      if (body.model !== model || body.stream !== false) throw failure('EVALUATION_MODEL_MISMATCH', 'Fixed non-streaming model required.');
      if (sampling.temperature !== undefined) body.temperature = sampling.temperature;
      if (sampling.seedSupported === true) body.seed = sampling.seed;
      const upstream = await fetch(connection.baseUrl + expectedPath, { method: 'POST', redirect: 'error',
        headers: { 'Content-Type': 'application/json', ...authorization(connection) }, body: JSON.stringify(body),
        signal: AbortSignal.timeout(sampling.timeoutMs) });
      const bytes = await readBounded(upstream.body);
      if (upstream.ok) {
        const result = JSON.parse(bytes.toString('utf8')), usage = result.usage ?? {};
        attempt.inputTokens = knownNumber(usage.prompt_tokens ?? usage.input_tokens);
        attempt.outputTokens = knownNumber(usage.completion_tokens ?? usage.output_tokens);
      } else attempt.errorCode = `UPSTREAM_HTTP_${upstream.status}`;
      response.writeHead(upstream.status, { 'Content-Type': 'application/json' }); response.end(bytes);
    } catch (error) {
      attempt.errorCode = typeof error.code === 'string' ? error.code : 'EVALUATION_TRANSPORT_FAILED';
      response.writeHead(502, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ error: attempt.errorCode }));
    } finally { attempt.durationMs = performance.now() - started; }
  });
  await new Promise((resolveReady, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolveReady); });
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, apiKey: transportKey,
    close: () => new Promise(resolveClosed => { server.close(resolveClosed); server.closeIdleConnections?.(); }) };
}

function restrictTools(tools, workspace, checks, checkRunner, variant, metrics) {
  const allowed = new Set(BASE_TOOLS);
  if (variant.gaps) allowed.add('knowledge.assess');
  if (variant.relations) allowed.add('knowledge.relations');
  if (variant.experience) allowed.add('knowledge.experience');
  const catalog = tools.catalog.bind(tools), execute = tools.execute.bind(tools);
  tools.listSkills = async () => [];
  tools.systemPrompt = async () => `Work only in the copied fixture. Do not access other paths, launch software, connect services or change validators.\n` +
    `Available checks: ${[...checks.keys()].join(', ')}. Use terminal.run with command node and args ["--test", "check-ID"] when validation is needed. ` +
    `Read sources and verify edits. Finish with the result and explicit limitations; independent evaluation runs afterwards.`;
  tools.catalog = async (context, options) => {
    const descriptors = (await catalog(context, { ...options, connectMcp: false })).filter(tool => allowed.has(tool.name));
    const snapshot = tools.catalogs.get(context);
    snapshot.descriptors = new Map([...descriptors, CHECK_DESCRIPTOR].map(tool => [tool.name, tool]));
    return [...descriptors, CHECK_DESCRIPTOR];
  };
  tools.execute = async (context, call, options = {}) => {
    metrics.sampleMemory();
    if (metrics.firstUsefulWorkMs === null) metrics.firstUsefulWorkMs = performance.now() - metrics.started;
    if (metrics.attempts.length) metrics.attempts.at(-1).toolCalls++;
    try {
      if (call.name === CHECK_DESCRIPTOR.name) {
        if (Object.keys(call.arguments).some(key => !['command', 'args'].includes(key)) || call.arguments.command !== 'node' ||
            !Array.isArray(call.arguments.args) || call.arguments.args.length !== 2 || call.arguments.args[0] !== '--test' ||
            !checks.has(call.arguments.args[1]))
          throw failure('EVALUATION_COMMAND_DENIED', 'Only manifest check IDs are allowed.');
        const value = await checkRunner(checks.get(call.arguments.args[1]), options.signal);
        return tools._finishResult(context, call, { value, isError: !value.passed });
      }
      if (!allowed.has(call.name)) throw failure('EVALUATION_TOOL_DENIED', 'Tool is outside the benchmark catalog.');
      if (call.name.startsWith('filesystem.')) {
        const path = inside(workspace, call.arguments.path ?? '.');
        await inspectLocalPath(path, { allowMissing: ['filesystem.write', 'filesystem.mkdir'].includes(call.name) });
      }
      return await execute(context, call, options);
    } catch (error) {
      if (/EVALUATION_(?:SCOPE|TOOL|COMMAND)_DENIED/u.test(error.code ?? '')) metrics.observedScopeViolations++;
      return tools._finishResult(context, call, { value: { code: error.code ?? 'EVALUATION_TOOL_FAILED' }, isError: true });
    }
  };
}

async function archiveSources(runtime, conversationId, bindings) {
  const messages = await runtime.conversations.readMessages(conversationId), sourceIds = [], budgets = [], archives = new Set();
  for (const message of messages.filter(item => item.Role === 'assistant')) {
    if (message.RetrievalResultRef) archives.add(message.RetrievalResultRef.id);
    for (const activity of message.ToolActivities ?? []) if (activity.name === 'knowledge.search' && activity.resultRef) archives.add(activity.resultRef.id);
  }
  for (const id of archives) {
    const archive = await runtime.tools.results.get({ conversationId }, id), payload = archive.structuredContent;
    if (!payload || !Array.isArray(payload.items)) continue;
    if (payload.budget) budgets.push(payload.budget);
    for (const item of payload.items) {
      const stableId = bindings.find(binding => binding.sourceId === item.sourceId)?.stableSourceId ?? item.sourceId;
      if (stableId && !sourceIds.includes(stableId)) sourceIds.push(stableId);
    }
  }
  return { sourceIds, budgets };
}

/** Run the actual gateway runtime in isolated copies; this adapter is an opt-in evaluation tool, never a production gateway.
 * 在隔离副本运行实际网关；本适配器仅由显式评测命令使用，不启动正式网关。 */
export async function createEvaluationAdapter(manifest, { environment = process.env, onDiagnostic } = {}) {
  const protocol = environment.KYNXA_EVAL_PROTOCOL ?? 'openai-completions';
  if (!protocols.includes(protocol)) throw failure('EVALUATION_PROTOCOL_UNSUPPORTED', 'Unsupported evaluation protocol.');
  let endpoint;
  try { endpoint = new URL(environment.KYNXA_EVAL_BASE_URL ?? ''); }
  catch { throw failure('EVALUATION_ENDPOINT_INVALID', 'Set KYNXA_EVAL_BASE_URL explicitly.'); }
  if (endpoint.username || endpoint.password || endpoint.search || endpoint.hash ||
      !['http:', 'https:'].includes(endpoint.protocol) || endpoint.protocol !== 'https:' && !isLocalEndpoint(endpoint))
    throw failure('EVALUATION_ENDPOINT_INVALID', 'Use an explicit HTTPS or local endpoint without credentials in its URL.');
  if (!isLocalEndpoint(endpoint) && !environment.KYNXA_EVAL_API_KEY)
    throw failure('EVALUATION_KEY_REQUIRED', 'Set KYNXA_EVAL_API_KEY in the environment.');
  if (typeof manifest.fixtureRoot !== 'string' || typeof manifest.validatorRoot !== 'string')
    throw failure('EVALUATION_FIXTURE_REQUIRED', 'Declare fixtureRoot and validatorRoot.');
  const checks = new Map();
  for (const input of manifest.acceptanceCommands ?? []) {
    const check = validateCheck(input);
    if (checks.has(check.id)) throw failure('EVALUATION_CHECK_INVALID', 'Duplicate validator ID.');
    checks.set(check.id, check);
  }
  const connection = validateConnection({ providerId: 'evaluation', displayName: 'Evaluation', protocol,
    baseUrl: endpoint.href.replace(/\/+$/u, ''), apiKey: environment.KYNXA_EVAL_API_KEY,
    models: [manifest.model], contextWindowTokens: manifest.contextWindowTokens ?? 131072, maxOutputTokens: manifest.maxOutputTokens ?? 8192 });
  let active = false, disposed = false;
  return {
    fixedModel: manifest.model, supportedAblations: RETRIEVAL_ABLATIONS.map(item => item.id),
    async run({ task, variant, seed, isolation }) {
      variant = validateVariant(variant);
      if (disposed || active) throw failure('EVALUATION_BUSY', 'Evaluations must be sequential and await executor shutdown.');
      if (isolation?.separateDataRoot !== true || isolation?.waitForPreviousExecutors !== true)
        throw failure('EVALUATION_ISOLATION_REQUIRED', 'Independent data and settled executors are required.');
      if (typeof task.query !== 'string' || !task.query || task.query.length > MAX_QUERY_CHARACTERS)
        throw failure('EVALUATION_QUERY_LIMIT', 'Query exceeds the gateway input contract; it cannot be truncated.');
      for (const id of task.acceptanceCheckIds) if (!checks.has(id)) throw failure('EVALUATION_CHECK_INVALID', 'Task refers to an undeclared validator.');
      active = true;
      // Node permissions compare canonical paths; Windows TEMP can contain a short 8.3 user alias.
      // Node 权限比较真实路径，而 Windows TEMP 可能含用户名的 8.3 短别名。
      let temporaryParent, temporary;
      try {
        temporaryParent = await realpath(tmpdir());
        temporary = await mkdtemp(join(temporaryParent, 'kynxa-unified-evaluation-'));
      } catch (error) { active = false; throw error; }
      const workspace = join(temporary, 'work'), validators = join(temporary, 'validators');
      let runtime, transport, authority;
      const metrics = { started: performance.now(), firstUsefulWorkMs: null, peakRssBytes: process.memoryUsage().rss,
        observedScopeViolations: 0, attempts: [] };
      metrics.sampleMemory = () => { metrics.peakRssBytes = Math.max(metrics.peakRssBytes, process.memoryUsage().rss); };
      try {
        const originals = await copyFixture(resolve(task.fixtureRoot ?? manifest.fixtureRoot), workspace);
        const originalValidators = await copyFixture(resolve(task.validatorRoot ?? manifest.validatorRoot), validators);
        const dataHome = join(temporary, 'Data', 'Models'), extensionRoot = join(temporary, 'Extensions');
        const conversations = new ConversationStore({ dataHome, legacyDesktopDirectory: null });
        const conversationId = randomUUID(), projectId = randomUUID(), requestId = randomUUID(), userMessageId = randomUUID();
        // The formal catalog intentionally drops empty drafts; seed the real current user message once.
        // 正式目录会丢弃空草稿，因此只预存本次真实用户消息，Runtime 通过同一 ID 复用。
        await conversations.saveCatalog({ ...(await conversations.catalog()), Projects: [{ Id: projectId, Name: 'Evaluation fixture',
          FolderPath: workspace, Chats: [{ Id: conversationId, Title: task.id, Messages: [{ Id: userMessageId, Role: 'user',
            Content: task.query, Status: 'completed', CreatedAt: new Date().toISOString() }] }] }], Chats: [] });
        authority = new ResourceBudgetService();
        const resources = evaluationResourceView(authority, variant.adaptiveResources);
        const tools = new ToolService({ conversationStore: conversations, dataHome, extensionRoot, bundledDirectory: null, officialTools: false });
        transport = await measuredTransport(connection, manifest.model, { temperature: manifest.sampling?.temperature,
          seedSupported: manifest.sampling?.seedSupported === true, seed, timeoutMs: manifest.requestTimeoutMs ?? 60000 }, metrics.attempts);
        const checkRunner = (check, signal) => executeCheck(check, workspace, validators, resources, originalValidators, signal);
        restrictTools(tools, workspace, checks, checkRunner, variant, metrics);
        const evaluationPolicy = { gaps: variant.gaps, relations: variant.relations, experience: variant.experience,
          ...(!variant.adaptiveResources ? { fixedBudget: FIXED_BUDGET } : {}) };
        runtime = new ModelRuntime({ modelStore: { connectionFor: async provider => provider === 'evaluation'
          ? { ...connection, apiKey: transport.apiKey, baseUrl: transport.baseUrl } : undefined }, dataHome, extensionRoot,
        conversationStore: conversations, toolService: tools, resourceService: resources, evaluationPolicy,
        timeoutMs: manifest.requestTimeoutMs ?? 60000, streamTimeoutMs: manifest.taskTimeoutMs ?? 300000 });
        if (runtime.retrieval.evaluationPolicy?.gaps !== variant.gaps)
          throw failure('EVALUATION_VARIANT_UNSUPPORTED', 'Runtime lacks an isolated evidence-policy hook.');
        await runtime.retrieval.settings.patchGlobal({ expectedRevision: 0, patch: { local: {
          semantic: manifest.retrieval?.semantic ?? 'off', embeddingProfileId: manifest.retrieval?.embeddingProfileId ?? 'builtin-multilingual',
          rerankProfileId: manifest.retrieval?.rerankProfileId ?? null, indexing: { batchSize: variant.adaptiveResources ? 32 : 8 } },
        web: { mode: 'off', browserRead: 'off' } } });
        const coldStartMs = performance.now() - metrics.started, indexingStarted = performance.now(), bindings = [];
        for (const source of task.sources ?? manifest.sources ?? []) {
          if (typeof source.id !== 'string' || !source.id || bindings.some(item => item.stableSourceId === source.id))
            throw failure('EVALUATION_SOURCE_INVALID', 'Fixture sources require unique stable IDs.');
          const path = inside(workspace, source.path);
          if (!(await lstat(path)).isFile()) throw failure('EVALUATION_SOURCE_INVALID', 'Each labeled source must be one file.');
          const imported = await runtime.retrieval.importSource({ path, scope: 'project', projectId });
          if (imported.importedCount !== 1) throw failure('EVALUATION_SOURCE_INVALID', 'Windowed multi-source files require an explicit benchmark adapter.');
          if (imported.jobId) await runtime.retrieval.activeJobs.get(imported.jobId)?.promise;
          const entry = (await runtime.retrieval.library.describeSources([`project:${projectId}`])).sources.find(item => item.sourceId === imported.id);
          bindings.push({ stableSourceId: source.id, sourceId: imported.id, contentHash: entry?.contentHash ?? digest(await readFile(path)),
            relativePath: relative(workspace, path).replaceAll('\\', '/') });
        }
        const indexPreparationMs = performance.now() - indexingStarted;
        const search = runtime.retrieval.search.bind(runtime.retrieval); let retrievalMs = 0;
        runtime.retrieval.search = async (...args) => {
          const started = performance.now(); metrics.sampleMemory();
          if (metrics.firstUsefulWorkMs === null) metrics.firstUsefulWorkMs = started - metrics.started;
          try { return await search(...args); } finally { retrievalMs += performance.now() - started; metrics.sampleMemory(); }
        };
        let errorCode;
        try {
          await runtime.reply({ conversationId, requestId, userMessageId, message: task.query, provider: 'evaluation', model: manifest.model,
            permissionMode: 'full', runLimits: manifest.runLimits ?? { maxRounds: 16, maxToolCalls: 64,
              maxGeneratedTokens: 131072, maxDurationMs: manifest.taskTimeoutMs ?? 300000 } });
        } catch (error) {
          errorCode = error.code ?? 'EVALUATION_AGENT_FAILED';
          onDiagnostic?.({ taskId: task.id, variant: variant.id, phase: 'agent', code: errorCode, message: error.message });
        }
        const archived = await archiveSources(runtime, conversationId, bindings), acceptanceReceipts = [];
        for (const id of task.acceptanceCheckIds) acceptanceReceipts.push({
          origin: 'independent-validator', ...await checkRunner(checks.get(id)) });
        await verifyFiles([...originals, ...originalValidators]); metrics.sampleMemory();
        const assistant = (await conversations.readMessages(conversationId)).find(item => item.Id === requestId);
        return { ...metrics, sampleMemory: undefined, attempts: [measuredAttempt(metrics.attempts)], modelRequests: metrics.attempts, sourceIds: archived.sourceIds,
          sourceBindings: bindings, acceptanceReceipts, verifiedSources: true, peakGpuBytes: null,
          assistantOutcome: { status: assistant?.Status ?? 'unavailable',
            segmentPhases: (assistant?.AssistantSegments ?? []).map(segment => segment.phase) },
          coldStartMs, indexPreparationMs, retrievalMs, modelMs: metrics.attempts.reduce((sum, item) => sum + item.durationMs, 0),
          estimatedGeneratedTokens: assistant?.ToolRun?.estimatedGeneratedTokens ?? null, errorCode,
          executionPolicy: { flags: { gaps: variant.gaps, adaptiveResources: variant.adaptiveResources,
            relations: variant.relations, experience: variant.experience }, fixedBudget: evaluationPolicy.fixedBudget ?? null,
            cpuThreadsLimit: variant.adaptiveResources ? null : 2, batchBudget: variant.adaptiveResources ? null : 8,
            resourceMode: authority.status().mode ?? null, retrievalBudgets: archived.budgets,
            sampling: { temperature: manifest.sampling?.temperature ?? null, seedRequested: manifest.sampling?.seedSupported === true,
              seed, providerDeterminismVerified: false }, transport: 'bounded-local-measurement-proxy' } };
      } finally {
        // Stop real workers before closing the authority or deleting copies; never touch a running user gateway.
        // 先关闭真实执行器再关闭资源服务和清理副本，完全不触碰用户正在运行的网关。
        try { await runtime?.close(); }
        finally {
          try { await transport?.close(); }
          finally {
            try { await authority?.close(); }
            finally {
              const suffix = relative(temporaryParent, resolve(temporary));
              if (!suffix || suffix === '..' || suffix.startsWith(`..${sep}`)) throw failure('EVALUATION_CLEANUP_SCOPE', 'Unsafe temporary cleanup.');
              try { await rm(temporary, { recursive: true, force: true }); }
              finally { active = false; }
            }
          }
        }
      }
    },
    async dispose() {
      if (active) throw failure('EVALUATION_BUSY', 'Wait for the running case and its executors before disposal.');
      disposed = true;
    }
  };
}
