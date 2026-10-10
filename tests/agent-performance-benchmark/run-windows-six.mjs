import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdir, readFile, readdir, readlink, stat, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { readUsage } from './evaluation.mjs';
import { WINDOWS_SIX_TASKS, WINDOWS_SIX_MODEL, WINDOWS_SIX_LIMITS, WINDOWS_SIX_ADAPTER_VERSION,
  WINDOWS_SIX_SCORING_VERSION, WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT, CHANGED_PENDING_WORK, PENDING_WORK_TEST,
  prepareSixWorkspace, verifySixTask, windowsSixPlan } from './windows-six-suite.mjs';

const REPOSITORY_ROOT = fileURLToPath(new URL('../../', import.meta.url));
const sha256 = value => createHash('sha256').update(value).digest('hex');
const failure = code => Object.assign(new Error(code), { code });
const safeCode = error => /^[A-Z0-9_]{1,120}$/.test(error?.code ?? '') ? error.code : 'SIX_EXECUTION_FAILED';
const within = (root, path) => { const suffix = relative(resolve(root), resolve(path));
  return suffix === '' || !isAbsolute(suffix) && suffix !== '..' && !suffix.startsWith(`..${sep}`); };
const TOOL_NAMES = new Set(['filesystem.read', 'filesystem.write', 'filesystem.edit', 'filesystem.list',
  'filesystem.stat', 'filesystem.search', 'knowledge.search', 'knowledge.read', 'knowledge.relations',
  'knowledge.assess', 'knowledge.plan', 'knowledge.experience', 'tool.search', 'tool.load', 'tool.result.read',
  'conversation.history.read', 'conversation.history.search', 'memory.read', 'terminal.run']);
const CRITICAL_CODES = new Set(['FIXTURE_SCOPE_VIOLATION',
  'TOOL_EXECUTION_RESULT_UNKNOWN', 'TOOL_RESULT_UNKNOWN', 'SQLITE_CORRUPT']);

export function assertPreparedRetryAdmission(taskId, prior, messages) {
  if (prior?.taskId !== taskId || !['setup-error', 'error', 'interrupted'].includes(prior.executionStatus) ||
      prior.closeErrorCode || prior.criticalFailure ||
      ['modelCalls', 'observations', 'formalToolActivities', 'answers'].some(field => !Array.isArray(prior[field])) ||
      prior.observations.length || prior.formalToolActivities.length ||
      prior.callAccounting?.modelRequests !== prior.modelCalls.length || prior.callAccounting?.formalToolAttempts !== 0 ||
      prior.answers.some(answer => !['error', 'interrupted'].includes(answer.status) || String(answer.content ?? '').trim()) ||
      prior.modelCalls.some(call => call.status === 'unknown' || (call.nativeToolCalls ?? 0) > 0 || call.finishReason === 'tool_calls' ||
        call.wireMessage?.tool_calls?.length) || !Array.isArray(messages) || !messages.length || messages.length > 9 ||
      messages.some(message => message.ToolActivities?.length || message.ModelTranscript?.rounds?.some(round => round.calls?.length || round.nativeContinuationRef) ||
        !['user', 'assistant'].includes(message.Role) || message.Role === 'assistant' &&
        (!['error', 'interrupted'].includes(message.Status) || String(message.Content ?? '').trim())))
    throw failure('SIX_PREPARED_RETRY_NOT_SAFE');
  if (messages.length > 3) {
    const instruction = WINDOWS_SIX_TASKS.find(task => task.id === taskId)?.instruction;
    const userMessages = messages.filter(message => message.Role === 'user'), originalTask = userMessages[1]?.Content;
    if (!instruction || userMessages[0]?.Content !== 'Windows isolated acceptance fixture.' ||
        typeof originalTask !== 'string' || !originalTask.startsWith(instruction) ||
        userMessages.slice(1).some(message => message.Content !== originalTask))
      throw failure('SIX_PREPARED_RETRY_NOT_SAFE');
  }
  return true;
}

async function observeNativeResourceSnapshot(resources) {
  const before = performance.now();
  try {
    const mode = resources.status?.().mode ?? 'unknown';
    if (mode !== 'rust') return { status: 'not-sampled-mode', mode, samplingLatencyMs: 0 };
    return { status: 'sampled', mode, snapshot: await resources.snapshot(),
      samplingLatencyMs: Math.ceil(performance.now() - before) };
  } catch (error) { return { status: 'sampling-failed', errorCode: safeCode(error),
    samplingLatencyMs: Math.ceil(performance.now() - before) }; }
}

export function observeSixResourceAcquisitions(resources, records) {
  const acquire = resources.acquire;
  // Observe the same native service only after denial; original requests, leases and errors remain untouched.
  // 仅在拒绝后观测同一个原生服务；不改原始请求、资源租约和抛出的错误对象。
  resources.acquire = async function (request, options) {
    const before = performance.now();
    const record = { number: records.length + 1, request: Object.fromEntries(['taskId', 'workspaceId', 'kind',
      'cpuThreads', 'memoryBytes', 'gpuMemoryBytes', 'waitMs', 'ttlMs'].map(key => [key, request?.[key]])) };
    records.push(record);
    try {
      const lease = await acquire.call(resources, request, options);
      record.acquireLatencyMs = Math.ceil(performance.now() - before);
      record.result = { status: lease?.status ?? 'unknown', reason: lease?.reason ?? null,
        mode: lease?.mode ?? null, leaseId: lease?.leaseId ?? null, cpuThreads: lease?.cpuThreads,
        memoryBytes: lease?.memoryBytes, gpuMemoryBytes: lease?.gpuMemoryBytes };
      if (lease?.status === 'denied') record.observation = await observeNativeResourceSnapshot(resources);
      return lease;
    } catch (error) {
      record.acquireLatencyMs = Math.ceil(performance.now() - before); record.errorCode = safeCode(error);
      record.observation = await observeNativeResourceSnapshot(resources);
      throw error;
    }
  };
}

export function normalizePreparedLinkTarget(root, linkPath, target, platform = process.platform) {
  if (platform !== 'win32') return target;
  // Windows copies may change separators; preserve the actual relative target and repository boundary.
  // Windows 复制链接可能改变分隔符；仅接受目标相同且仍位于本仓库内的相对链接。
  const normalizedTarget = target.replaceAll('\\', '/');
  if (!normalizedTarget || isAbsolute(normalizedTarget) ||
      !within(root, resolve(root, dirname(linkPath), normalizedTarget)))
    throw failure('SIX_PREPARED_WORK_LINK_OUTSIDE_REPOSITORY');
  return normalizedTarget;
}

export function assertPreparedManifestMatches(actual, expected) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw failure('SIX_PREPARED_WORK_CHANGED');
}

async function fixtureTree(root, suffix = '') {
  const records = [], entries = await readdir(join(root, suffix), { withFileTypes: true });
  entries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    const path = suffix ? `${suffix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) records.push(...await fixtureTree(root, path));
    else if (entry.isFile()) { const bytes = await readFile(join(root, path)); records.push({ path, bytes: bytes.length, sha256: sha256(bytes) }); }
    else if (entry.isSymbolicLink()) records.push({ path,
      link: normalizePreparedLinkTarget(root, path, await readlink(join(root, path))) });
    else throw failure('SIX_PREPARED_WORK_FILE_TYPE_CHANGED');
  }
  return records;
}

export async function readPreparedFixture(task, options) {
  const taskRoot = resolve(options.preparedRoot), workspace = join(taskRoot, 'Work');
  if (!within(join(REPOSITORY_ROOT, 'artifacts'), taskRoot)) throw failure('SIX_PREPARED_ROOT_OUTSIDE_ARTIFACTS');
  const priorBytes = await readFile(join(taskRoot, 'result.json')), prior = JSON.parse(priorBytes);
  const actual = await fixtureTree(workspace);
  let message = task.instruction, expected;
  if (task.instanceId) {
    const raw = await readFile(join(options.cacheRoot, 'test/agent-benchmark-20261008/stratified-subset30-v2/model-public-cases.json'));
    if (sha256(raw) !== prior.fixture?.publicCaseFileHash) throw failure('SIX_PUBLIC_CASE_FILE_CHANGED');
    const instance = JSON.parse(raw).cases.find(item => item.instance_id === task.instanceId);
    if (!instance || instance.base_commit !== task.commit || sha256(instance.problem_statement) !== prior.fixture.issueHash)
      throw failure('SIX_PUBLIC_CASE_IDENTITY_INVALID');
    const cache = join(options.cacheRoot, 'test/formal-source-cache/fixtures', `${task.repo.replace('/', '__')}--${task.commit}`);
    const directory = (await readdir(cache, { withFileTypes: true })).find(item => item.isDirectory() && item.name.endsWith(task.commit));
    if (!directory) throw failure('SIX_SOURCE_CACHE_MISSING');
    expected = await fixtureTree(join(cache, directory.name));
    message += `\n\n公开问题原文：\n${instance.problem_statement}`;
  } else if (task.type === 'small-repair') expected = [
    { path: 'lib/pending-work.mjs', bytes: Buffer.byteLength(CHANGED_PENDING_WORK), sha256: prior.fixture.changedHash },
    { path: 'pending-work.test.mjs', bytes: Buffer.byteLength(PENDING_WORK_TEST), sha256: prior.fixture.testHash }];
  else if (['document-question', 'insufficient-evidence'].includes(task.type)) expected = [
    { path: 'scifact-31715818.md', bytes: actual.find(item => item.path === 'scifact-31715818.md')?.bytes,
      sha256: prior.fixture.sourceHash }];
  else { const content = 'This work uses its current confirmed project memory as the delivery identifier.\n';
    expected = [{ path: 'delivery.md', bytes: Buffer.byteLength(content), sha256: sha256(content) }]; }
  assertPreparedManifestMatches(actual, expected);
  return { taskRoot, prior, priorResultHash: sha256(priorBytes), fixture: { message, provenance: prior.fixture },
    workManifestHash: sha256(JSON.stringify(actual)), workFileCount: actual.length };
}

// Load each frozen product tree through the same external evaluator; do not modify its source or score rules.
// 两个冻结产品树共用同一外部评测入口，不修改产品源码或针对某版更改评分。
export async function loadSixProduct(sourceRoot) {
  const module = path => import(pathToFileURL(join(resolve(sourceRoot), 'apps/model-gateway', path)).href);
  const [{ ModelRuntime }, { ConversationStore }, { MemoryService }, { ToolService }, { SandboxRunner },
    { EmbeddingService }] = await Promise.all([module('orchestration/runtime.mjs'), module('data/conversations.mjs'),
    module('data/memory-service.mjs'), module('tools/tool-service.mjs'), module('tools/sandbox-runner.mjs'),
    module('models/retrieval/embedding-service.mjs')]);
  return { ModelRuntime, ConversationStore, MemoryService, ToolService, SandboxRunner, EmbeddingService };
}

async function independentNodeTest(workspace) {
  const started = performance.now();
  return new Promise(resolveTest => {
    const child = spawn(process.execPath, ['--test', '--test-isolation=none', 'pending-work.test.mjs'],
      { cwd: workspace, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout = (stdout + chunk).slice(-65536); });
    child.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-65536); });
    const timer = setTimeout(() => child.kill(), 30000);
    child.once('error', error => { clearTimeout(timer); resolveTest({ exitCode: null, errorCode: safeCode(error),
      durationMs: Math.ceil(performance.now() - started), stdout, stderr }); });
    child.once('exit', (exitCode, signal) => { clearTimeout(timer); resolveTest({ exitCode, signal,
      durationMs: Math.ceil(performance.now() - started), stdout, stderr }); });
  });
}

async function indexSnapshot(runtime, jobId) {
  const [index, job] = await Promise.all([runtime.retrieval.index.status(),
    jobId ? runtime.retrieval.jobs.get(jobId) : null]);
  return { index, job, embedding: runtime.retrieval.embeddings.status() };
}

export function sixActionReadiness(task, { filesReadable, lexicalPaths = [] } = {}) {
  const required = task.type === 'memory-and-scope' ? 'confirmed-memory'
    : ['small-repair', 'document-question'].includes(task.type) ? 'target-lexical-text' : 'workspace-read';
  const lexicalReady = task.requiredFiles.every(path => lexicalPaths.some(value => value === path || value.endsWith(`/${path}`)));
  return { required, available: required === 'confirmed-memory' || filesReadable === true,
    preferredReady: required !== 'target-lexical-text' || lexicalReady,
    effective: required === 'target-lexical-text' && !lexicalReady ? 'workspace-read' : required,
    semanticCompletionRequired: false };
}

async function awaitActionReadiness(runtime, task, { jobId, workspace, projectId, deadline }) {
  const readable = await Promise.all(task.requiredFiles.map(async path => (await stat(join(workspace, path))).isFile()));
  const filesReadable = readable.every(Boolean), graceDeadline = Math.min(deadline, performance.now() + 2000);
  if (!filesReadable) throw failure('SIX_REQUIRED_WORKSPACE_FILE_UNAVAILABLE');
  for (;;) {
    runtime.shutdown.signal.throwIfAborted();
    const sources = await runtime.retrieval.index.listSources({ scopeKeys: [`project:${projectId.toLowerCase()}`] });
    const lexicalPaths = sources.filter(source => source.chunkCount > 0)
      .map(source => String(source.locator?.relativePath ?? '').replaceAll('\\', '/'));
    const readiness = sixActionReadiness(task, { filesReadable, lexicalPaths });
    if (readiness.preferredReady || performance.now() >= graceDeadline) return {
      ...await indexSnapshot(runtime, jobId), readiness,
      indexPreparationGraceMs: 2000,
      coverageNote: 'Action admission does not certify whole-repository lexical or semantic coverage.' };
    await new Promise(resolveDelay => setTimeout(resolveDelay, Math.min(50, graceDeadline - performance.now())));
  }
}

export function observeSixDocumentEmbeddings(embeddings, records, documentLimit = WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT) {
  if (!Number.isSafeInteger(documentLimit) || documentLimit < 1 || documentLimit > 100000)
    throw failure('SIX_INVALID_DOCUMENT_EMBEDDING_LIMIT');
  const embedDocuments = embeddings.embedDocuments;
  let admittedDocuments = 0;
  embeddings.embedDocuments = async function (documents, options) {
    const record = { number: records.length + 1, requestedDocuments: documents.length,
      documentLimit, admittedBefore: admittedDocuments, status: 'running' };
    records.push(record);
    if (documents.length > documentLimit - admittedDocuments) {
      record.status = 'budget-exhausted'; record.errorCode = 'SIX_DOCUMENT_EMBEDDING_BUDGET_REACHED';
      throw failure(record.errorCode);
    }
    // Reuse remains production-owned; this budget admits real increments instead of refusing every cache miss.
    // 缓存复用仍由产品负责；此预算允许真实增量，不再把每个缓存未命中都直接拒绝。
    admittedDocuments += documents.length;
    const before = performance.now();
    try {
      const result = await embedDocuments.call(this, documents, options);
      record.status = 'completed'; record.completedVectors = result.vectors?.filter(Boolean).length ?? 0;
      return result;
    } catch (error) { record.status = 'failed'; record.errorCode = safeCode(error); throw error; }
    finally { record.durationMs = Math.ceil(performance.now() - before); }
  };
}

export function sentAutomaticEvidenceRecords(modelCalls) {
  const records = [];
  for (const call of modelCalls) {
    if (!(call.httpStatus >= 200 && call.httpStatus < 300)) continue;
    for (const message of call.syntheticLocalRequest?.messages ?? []) {
      if (!['system', 'developer'].includes(message.role) || typeof message.content !== 'string') continue;
      for (const line of message.content.split(/\r?\n/u)) {
        try {
          const record = JSON.parse(line);
          if (typeof record?.sourceRef === 'string' && typeof record.excerpt === 'string' && record.excerpt.trim() && !record.navigationOnly)
            records.push({ ...record, modelCallNumber: call.number });
        } catch { /* Non-evidence prompt text is not a source receipt. / 普通提示文字不算来源回执。 */ }
      }
    }
  }
  return records;
}

async function collectAutomaticEvidence(runtime, tools, assistants, modelCalls, { workspace, projectId, conversationId }) {
  const collected = [], sent = sentAutomaticEvidenceRecords(modelCalls);
  for (const assistant of assistants) {
    if (!assistant.RetrievalResultRef?.id) continue;
    const context = { conversationId, requestId: assistant.Id, projectId };
    try {
      const archived = await tools.results.get(context, assistant.RetrievalResultRef.id);
      const library = await runtime.retrieval.library.list(projectId);
      for (const item of archived.structuredContent?.items ?? []) {
        const reference = assistant.EvidenceReferences?.find(value => value.sourceRef === item.sourceRef);
        const delivered = reference && sent.find(value => value.sourceRef === (reference.modelSourceRef ?? reference.sourceRef) &&
          value.excerpt === item.excerpt);
        if (!delivered) continue;
        try {
          const scopeKeys = ['user', `project:${projectId.toLowerCase()}`, `chat:${conversationId.toLowerCase()}`];
          if (typeof runtime.retrieval.index.verifyReference === 'function') {
            if (!(await runtime.retrieval.index.verifyReference({ sourceRef: item.sourceRef, scopeKeys })).current) continue;
          } else await runtime.retrieval.index.read({ sourceRef: item.sourceRef, scopeKeys, offset: 0, limit: 1 });
          let text, path = item.locator?.relativePath, contentHash;
          if (item.sourceType === 'knowledge') {
            const source = await runtime.retrieval.library.readSource(item.sourceId, { scopeKeys, sourceRevision: item.sourceRevision });
            text = source.text; contentHash = source.contentHash;
            path = library.sources.find(source => source.id === item.sourceId)?.path ?? path;
          } else if (item.sourceType === 'work-file') {
            const target = resolve(workspace, item.locator?.path ?? path ?? '');
            if (!within(workspace, target)) continue;
            text = await readFile(target, 'utf8'); contentHash = sha256(text); path = target;
          } else continue;
          const offset = item.locator?.startOffset;
          if (contentHash !== item.contentHash || !Number.isSafeInteger(offset) || offset < 0 ||
              text.slice(offset, offset + item.excerpt.length) !== item.excerpt) continue;
          let coverageStartOffset = offset, coverageEndOffset = offset + item.excerpt.length;
          while (coverageStartOffset > 0 && /\s/u.test(text[coverageStartOffset - 1])) coverageStartOffset--;
          while (coverageEndOffset < text.length && /\s/u.test(text[coverageEndOffset])) coverageEndOffset++;
          collected.push({ channel: 'automatic-evidence', deliveredToModel: true, sourceValidated: true,
            sourceRef: item.sourceRef, sourceId: item.sourceId, sourceRevision: item.sourceRevision, contentHash,
            relativePath: isAbsolute(path) ? relative(workspace, path).replaceAll('\\', '/') : path,
            text: item.excerpt, textHash: sha256(item.excerpt), contentCharacters: item.excerpt.trim().length,
            offset, totalCharacters: text.length, coverageStartOffset, coverageEndOffset,
            modelCallNumber: delivered.modelCallNumber });
        } catch (error) { collected.push({ channel: 'automatic-evidence', sourceValidated: false, errorCode: safeCode(error) }); }
      }
    } catch (error) { collected.push({ channel: 'automatic-evidence', sourceValidated: false, errorCode: safeCode(error) }); }
  }
  return collected;
}

export async function runSixTask(product, task, options) {
  const prepared = options.preparedRoot ? await readPreparedFixture(task, options) : null;
  const resultRoot = join(options.output, task.id), taskRoot = prepared?.taskRoot ?? resultRoot, workspace = join(taskRoot, 'Work');
  await mkdir(resultRoot, { recursive: true });
  const fixture = prepared?.fixture ?? await prepareSixWorkspace(task, workspace, options.cacheRoot);
  await writeFile(join(resultRoot, 'fixture-provenance.json'), JSON.stringify(fixture.provenance, null, 2));
  const dataHome = join(taskRoot, 'Data', 'Models'), extensionRoot = join(taskRoot, 'Extensions');
  if (!prepared) await mkdir(extensionRoot);
  const conversations = new product.ConversationStore({ dataHome, legacyDesktopDirectory: null });
  let projectId = randomUUID(), conversationId = randomUUID(), secondConversationId = randomUUID();
  let otherProjectId = randomUUID(), otherConversationId = randomUUID();
  const otherWorkspace = join(taskRoot, 'OtherWork');
  if (!prepared) await mkdir(otherWorkspace);
  const initialMessage = () => ({ Id: randomUUID(), Role: 'user', Status: 'completed', Content: 'Windows isolated acceptance fixture.' });
  if (!prepared) await conversations.saveCatalog({ Revision: (await conversations.catalog()).Revision, Projects: [
    { Id: projectId, Name: 'Six-task work', FolderPath: workspace, Chats: [
      { Id: conversationId, Title: task.id, Messages: [initialMessage()] },
      { Id: secondConversationId, Title: 'Same work secondary chat', Messages: [initialMessage()] }] },
    { Id: otherProjectId, Name: 'Other isolated work', FolderPath: otherWorkspace,
      Chats: [{ Id: otherConversationId, Title: 'Other work', Messages: [initialMessage()] }] }], Chats: [] });
  else {
    const catalog = await conversations.catalog(), project = catalog.Projects.find(item => resolve(item.FolderPath ?? '.') === workspace);
    const chat = project?.Chats.find(item => item.Title === task.id), other = catalog.Projects.find(item => resolve(item.FolderPath ?? '.') === otherWorkspace);
    if (!chat || !other) throw failure('SIX_PREPARED_CATALOG_CHANGED');
    projectId = project.Id; conversationId = chat.Id; secondConversationId = project.Chats.find(item => item.Id !== chat.Id)?.Id;
    otherProjectId = other.Id; otherConversationId = other.Chats[0]?.Id;
    const priorMessages = await conversations.readModelMessages(conversationId);
    assertPreparedRetryAdmission(task.id, prepared.prior, priorMessages);
    prepared.priorAssistantErrors = priorMessages.filter(item => item.Role === 'assistant').map(item => item.Error ?? null);
    prepared.emptyFailureHistory = priorMessages.filter(item => item.Role === 'assistant').map(item => ({
      messageId: item.Id, status: item.Status, error: item.Error ?? null, durationMs: item.DurationMs ?? null,
      modelUsage: 'unknown-from-formal-history', countedInThisRunAccounting: false }));
  }
  const memory = new product.MemoryService({ conversationStore: conversations });
  let runtime, started, deadline, setupErrorCode, errorCode, closeErrorCode, finalNodeTest, indexAtStart, indexBeforeChange;
  let memoryReceipt, criticalFailure, timer, finished, corpusCoverage, activeJobId, indexAtFirstAction,
    preCloseResourceSnapshot, actionAdmission;
  const observations = [], modelCalls = [], retrievalCalls = [], assistants = [], resourceAcquisitions = [];
  const documentEmbeddingCalls = [];
  let automaticEvidence = [];
  const baseFetch = globalThis.fetch;
  let firstEffectiveActionMs = null, firstEvidenceResultMs = null;
  const allowed = name => TOOL_NAMES.has(name) && (task.type === 'small-repair' || !['terminal.run', 'filesystem.write', 'filesystem.edit'].includes(name));
  class SixToolService extends product.ToolService {
    async catalog(context, catalogOptions) {
      const descriptors = await super.catalog(context, catalogOptions);
      const snapshot = context && this.catalogs.get(context);
      if (snapshot) {
        snapshot.descriptors = new Map([...snapshot.descriptors].filter(([name]) => allowed(name)));
        if (snapshot.inventory) snapshot.inventory = new Map([...snapshot.inventory].filter(([name]) => allowed(name)));
      }
      return descriptors.filter(item => allowed(item.name));
    }
    async execute(context, call, executionOptions) {
      const before = performance.now();
      const item = { number: observations.length + 1, name: call.name, arguments: structuredClone(call.arguments),
        startedMs: started === undefined ? null : Math.ceil(before - started), status: 'running', dispatched: false };
      observations.push(item);
      try {
        if (!allowed(call.name)) throw failure('SIX_TOOL_NOT_ALLOWED');
        if (call.name.startsWith('filesystem.')) {
          const target = resolve(workspace, call.arguments?.path ?? '.');
          if (!within(workspace, target)) throw failure('FIXTURE_SCOPE_VIOLATION');
          if (['filesystem.write', 'filesystem.edit'].includes(call.name) && target !== join(workspace, 'lib', 'pending-work.mjs'))
            throw failure('SIX_TEST_OR_OTHER_FILE_WRITE_DENIED');
        }
        if (call.name === 'terminal.run') {
          const command = String(call.arguments?.command ?? '').toLowerCase();
          const arguments_ = call.arguments?.args ?? [];
          if (!['node', 'node.exe'].includes(command) || JSON.stringify(arguments_) !==
              JSON.stringify(['--test', '--test-isolation=none', 'pending-work.test.mjs']))
            throw failure('SIX_TERMINAL_COMMAND_NOT_ALLOWED');
        }
        item.dispatched = true;
        if (indexAtFirstAction === undefined && ['filesystem.read', 'filesystem.search', 'knowledge.search', 'knowledge.read'].includes(call.name))
          indexAtFirstAction = activeJobId ? await runtime.retrieval.jobs.get(activeJobId) : null;
        const result = await super.execute(context, call, executionOptions);
        item.status = result.isError ? 'error' : result.status ?? 'completed';
        item.code = result.code ?? null; item.result = result.content; item.sandbox = result.sandbox;
        item.executionEnvironment = result.executionEnvironment;
        if (CRITICAL_CODES.has(item.code) || item.status === 'unknown') {
          criticalFailure = { tool: call.name, code: item.code }; runtime.shutdown.abort();
        }
        if (item.status === 'completed' && ['filesystem.read', 'knowledge.read'].includes(call.name)) {
          try {
            const archive = result.resultRef?.id ? await this.results.get(context, result.resultRef.id) : null;
            const value = archive?.structuredContent ?? JSON.parse(result.content);
            const text = value.text ?? value.content;
            let path = value.path ?? value.locator?.path ?? value.locator?.relativePath;
            if (value.sourceType === 'knowledge' && value.sourceId) {
              const library = await runtime.retrieval.library.list(projectId);
              path = library.sources.find(source => source.id === value.sourceId)?.path ?? path;
            }
            if (typeof text === 'string' && typeof path === 'string') item.readReceipt = {
              relativePath: isAbsolute(path) ? relative(workspace, path) : path, contentCharacters: text.trim().length,
              text, offset: value.offset ?? value.locator?.startOffset, totalCharacters: value.totalCharacters,
              textHash: sha256(text), sourceRevision: value.sourceRevision ?? null, contentHash: value.contentHash ?? value.sha256 ?? null };
          } catch (error) { item.receiptObservationError = safeCode(error); }
        }
        if (item.status === 'completed' && ['filesystem.read', 'knowledge.read', 'filesystem.search', 'knowledge.search'].includes(call.name)) {
          firstEffectiveActionMs ??= item.startedMs;
          if (['filesystem.read', 'knowledge.read'].includes(call.name))
            firstEvidenceResultMs ??= Math.ceil(performance.now() - started);
        }
        return result;
      } catch (error) {
        item.status = 'error'; item.code = safeCode(error);
        if (CRITICAL_CODES.has(item.code)) { criticalFailure = { tool: call.name, code: item.code }; runtime.shutdown.abort(); }
        throw error;
      } finally { item.durationMs = Math.ceil(performance.now() - before); }
    }
  }
  const tools = new SixToolService({ conversationStore: conversations, memoryService: memory, dataHome, extensionRoot,
    extensionPointer: join(taskRoot, 'unused-extension-pointer.json'), officialTools: false, bundledDirectory: null,
    sandboxRunner: options.toolHost ? new product.SandboxRunner({ toolHostPath: options.toolHost,
      excludedRoots: [dataHome, extensionRoot], conversationWorkspaceHome: dataHome }) : undefined });
  const selectedModel = options.model ?? WINDOWS_SIX_MODEL.model;
  const connection = { providerId: 'acceptance-qwen', displayName: 'Isolated local Qwen', protocol: 'openai-completions',
    baseUrl: options.endpoint, models: [selectedModel], contextWindowTokens: WINDOWS_SIX_MODEL.contextWindowTokens,
    maxOutputTokens: WINDOWS_SIX_MODEL.maxOutputTokens };
  const setupStarted = performance.now();
  try {
    runtime = new product.ModelRuntime({ modelStore: { connectionFor: async () => connection }, dataHome, extensionRoot,
      conversationStore: conversations, memoryService: memory, toolService: tools,
      timeoutMs: WINDOWS_SIX_LIMITS.maxDurationMs, streamTimeoutMs: WINDOWS_SIX_LIMITS.maxDurationMs });
    observeSixResourceAcquisitions(runtime.resources, resourceAcquisitions);
    // Supply the same read-only E5 assets to both versions; this changes location, not inference or retrieval logic.
    // 两版共用同一只读 E5 资产位置，只适配路径，不替换推理或检索算法。
    if (options.embeddingRoot) runtime.retrieval.embeddings.factory = arguments_ =>
      new product.EmbeddingService({ ...arguments_, modelRoot: options.embeddingRoot, devicePreference: 'cpu' });
    observeSixDocumentEmbeddings(runtime.retrieval.embeddings, documentEmbeddingCalls, options.documentEmbeddingLimit);
    if (prepared) {
      const effective = await runtime.retrieval.effective(projectId);
      if (effective.local.embeddingDevicePolicy !== 'cpu' || effective.local.embeddingProfileId !== 'builtin-multilingual')
        throw failure('SIX_PREPARED_RETRIEVAL_SETTINGS_CHANGED');
      corpusCoverage = prepared.prior.corpusCoverage; indexBeforeChange = prepared.prior.indexBeforeChange;
      activeJobId = prepared.prior.indexAtStart?.job?.jobId ?? prepared.prior.corpusCoverage?.imports?.[0]?.jobId;
      if (!activeJobId) activeJobId = (await runtime.retrieval.jobs.list()).findLast(job => job.projectId === projectId)?.jobId;
      indexAtStart = await awaitActionReadiness(runtime, task, { jobId: activeJobId, workspace, projectId,
        deadline: performance.now() + WINDOWS_SIX_LIMITS.maxDurationMs });
      actionAdmission = indexAtStart.readiness;
      if (task.type === 'memory-and-scope') {
        const same = await memory.contextFor(secondConversationId), other = await memory.contextFor(otherConversationId);
        memoryReceipt = { ...prepared.prior.memoryReceipt, scopeCheck: prepared.prior.memoryReceipt?.scopeCheck === true &&
          JSON.stringify(same).includes('BETA_8426') && !JSON.stringify(same).includes('ALPHA_7091') &&
          !JSON.stringify(same).includes('PRIVATE_OTHER_6631') && !JSON.stringify(other).includes('BETA_8426') };
      }
    } else {
    const global = await runtime.retrieval.settings.getGlobal();
    await runtime.retrieval.settings.patchGlobal({ expectedRevision: global.revision,
      patch: { local: { enabled: true, semantic: options.semantic ?? 'auto', embeddingDevicePolicy: 'cpu', rerankProfileId: null },
        web: { mode: 'off', browserRead: 'off' } } });
    const project = await runtime.retrieval.settings.getProject(projectId);
    await runtime.retrieval.settings.patchProject(projectId, { expectedRevision: project.revision,
      patch: { indexingSources: { mountedFolder: { enabled: !task.instanceId } } } });
    // Keep the full public repository available to file tools, while indexing disclosed task modules only.
    // 文件工具仍可读取完整公开仓库；索引仅覆盖明确披露的任务模块，不宣称整个仓库已就绪。
    if (task.instanceId) {
      const modulePaths = task.instanceId.startsWith('astropy') ? ['astropy/modeling'] : ['django/forms', 'tests/forms_tests'];
      const receipts = [];
      for (const path of modulePaths) receipts.push(await runtime.retrieval.importSource({
        path: join(workspace, path), scope: 'project', projectId }, { permissionMode: 'full' }));
      corpusCoverage = { kind: 'task-modules', modulePaths, fullRepositoryAvailableToFileTools: true,
        fullRepositorySemanticallyIndexed: false, imports: receipts.map(item => ({ importedCount: item.importedCount,
          sourceCoverage: item.sourceCoverage, jobId: item.jobId })) };
    } else corpusCoverage = { kind: 'complete-small-task-workspace', fullRepositorySemanticallyIndexed: null };
    if (task.type === 'memory-and-scope') {
      // A user-confirmed update uses existing APIs in both versions, not a new-only model tool.
      // 显式用户确认更新在两版均使用已有 API，不以新版专属模型工具制造对照差异。
      const original = await memory.create(conversationId, { scope: 'project', content: '本工作的交付代号是 ALPHA_7091。', kind: 'fact', expectedRevision: 0 });
      const job = await runtime.retrieval.rebuild({ projectId });
      activeJobId = job.jobId;
      indexBeforeChange = await awaitActionReadiness(runtime, task, { jobId: job.jobId, workspace, projectId,
        deadline: performance.now() + WINDOWS_SIX_LIMITS.maxDurationMs });
      actionAdmission = indexBeforeChange.readiness;
      const target = original.entries.find(entry => entry.content.includes('ALPHA_7091'));
      const updated = await memory.update(conversationId, target.id, { scope: 'project', expectedRevision: original.revision,
        content: '本工作的交付代号是 BETA_8426。' });
      await memory.create(otherConversationId, { scope: 'project', content: '另一个工作的私有交付代号是 PRIVATE_OTHER_6631。', kind: 'fact', expectedRevision: 0 });
      const sameWork = await memory.contextFor(secondConversationId), otherWork = await memory.contextFor(otherConversationId);
      memoryReceipt = { originalRevision: original.revision, updatedRevision: updated.revision, scopeId: updated.scopeId,
        scopeCheck: original.scopeId === projectId && updated.scopeId === projectId &&
          JSON.stringify(sameWork).includes('BETA_8426') && !JSON.stringify(sameWork).includes('ALPHA_7091') &&
          !JSON.stringify(sameWork).includes('PRIVATE_OTHER_6631') && !JSON.stringify(otherWork).includes('BETA_8426') };
      indexAtStart = await indexSnapshot(runtime, job.jobId);
    } else {
      const job = await runtime.retrieval.rebuild({ projectId });
      activeJobId = job.jobId;
      if (task.indexState !== 'partial') {
        const ready = await awaitActionReadiness(runtime, task, { jobId: job.jobId, workspace, projectId,
          deadline: performance.now() + WINDOWS_SIX_LIMITS.maxDurationMs });
        actionAdmission = ready.readiness;
        if (task.indexState === 'changed') {
          indexBeforeChange = ready;
          await writeFile(join(workspace, 'lib/pending-work.mjs'), CHANGED_PENDING_WORK);
          const beforeTest = await independentNodeTest(workspace);
          if (beforeTest.exitCode === 0) throw failure('SIX_REPAIR_FIXTURE_NOT_BROKEN');
        }
      }
      indexAtStart = await indexSnapshot(runtime, job.jobId);
      indexAtStart.plannedStateRealized = task.indexState !== 'partial' || indexAtStart.job?.status !== 'completed';
      if (!indexAtStart.plannedStateRealized) indexAtStart.stateDiagnostic = 'SIX_PARTIAL_STATE_FINISHED_BEFORE_REQUEST';
      actionAdmission ??= sixActionReadiness(task, { filesReadable: true });
    }
    }
    const search = runtime.retrieval.search.bind(runtime.retrieval);
    runtime.retrieval.search = async (...arguments_) => {
      const before = performance.now();
      try {
        const result = await search(...arguments_);
        if (started !== undefined && result.items?.some(item => !item.navigationOnly && item.excerpt?.trim())) {
          firstEffectiveActionMs ??= Math.ceil(before - started);
          firstEvidenceResultMs ??= Math.ceil(performance.now() - started);
        }
        retrievalCalls.push({ startedMs: started === undefined ? null : Math.ceil(before - started),
          durationMs: Math.ceil(performance.now() - before), strategy: result.strategy,
          itemCount: result.items?.length ?? 0, vectorAvailable: result.vectorAvailable, budget: result.budget,
          sourceScan: result.sourceScan, evidenceAssessment: result.evidenceAssessment });
        return result;
      } catch (error) { retrievalCalls.push({ durationMs: Math.ceil(performance.now() - before), errorCode: safeCode(error) }); throw error; }
    };
    started = performance.now(); deadline = started + WINDOWS_SIX_LIMITS.maxDurationMs;
    timer = setTimeout(() => runtime.shutdown.abort(), WINDOWS_SIX_LIMITS.maxDurationMs);
    globalThis.fetch = async (url, requestOptions = {}) => {
      const parsed = new URL(url), endpoint = new URL(options.endpoint);
      if (parsed.origin !== endpoint.origin) throw failure('SIX_NETWORK_DESTINATION_DENIED');
      if (!parsed.pathname.endsWith('/chat/completions')) return baseFetch(url, requestOptions);
      if (modelCalls.length >= WINDOWS_SIX_LIMITS.maxRounds) throw failure('SIX_MODEL_ROUND_LIMIT');
      const payload = JSON.parse(requestOptions.body);
      payload.temperature = WINDOWS_SIX_MODEL.temperature; payload.seed = WINDOWS_SIX_MODEL.seed; payload.think = false;
      payload.reasoning_effort = 'none';
      const lastUser = payload.messages.findLast(item => item.role === 'user');
      if (typeof lastUser?.content === 'string' && !lastUser.content.includes('/no_think')) lastUser.content += '\n/no_think';
      payload.max_tokens = WINDOWS_SIX_MODEL.maxOutputTokens;
      const item = { number: modelCalls.length + 1, status: 'running', inputMessages: payload.messages.length,
        maxOutputTokens: payload.max_tokens, usage: readUsage('openai-completions', null) };
      item.syntheticLocalRequest = payload;
      modelCalls.push(item); const before = performance.now();
      try {
        const observeModel = async () => {
          try {
            const response = await baseFetch(new URL('/api/ps', endpoint), { signal: AbortSignal.timeout(5000) });
            const current = (await response.json()).models?.find(model => model.name === selectedModel || model.model === selectedModel);
            return current ? { name: current.name, digest: current.digest, sizeBytes: current.size,
              gpuBytes: current.size_vram, contextWindowTokens: current.context_length ?? null,
              quantization: current.details?.quantization_level ?? null, expiresAt: current.expires_at ?? null } : { state: 'not-loaded' };
          } catch (error) { return { state: 'observation-unavailable', code: safeCode(error) }; }
        };
        item.modelBeforeDispatch = await observeModel();
        item.hostMemoryBeforeDispatch = { processRssBytes: process.memoryUsage().rss };
        const response = await baseFetch(url, { ...requestOptions, body: JSON.stringify(payload),
          signal: AbortSignal.any([...(requestOptions.signal ? [requestOptions.signal] : []),
            AbortSignal.timeout(Math.max(1, Math.floor(deadline - performance.now())))]) });
        item.httpStatus = response.status;
        const raw = await response.clone().json().catch(() => null);
        // Retain synthetic local-model wire receipts so empty replies can be attributed without guessing.
        // 保留合成题本地模型协议回执，便于定位空回复，不能凭生成 token 数猜测原因。
        item.wireMessage = raw?.choices?.[0]?.message ?? null;
        item.usage = readUsage('openai-completions', raw); item.status = response.ok ? 'completed' : 'http-error';
        item.finishReason = raw?.choices?.[0]?.finish_reason ?? null;
        item.nativeToolCalls = raw?.choices?.[0]?.message?.tool_calls?.length ?? 0;
        const message = raw?.choices?.[0]?.message;
        item.reasoningObservation = { requestedDisabled: true,
          reasoningCharacters: String(message?.reasoning_content ?? message?.reasoning ?? '').length,
          reasoningTagInContent: /<think>/i.test(message?.content ?? ''),
          reportedReasoningTokens: raw?.usage?.completion_tokens_details?.reasoning_tokens ?? null };
        item.modelAfterDispatch = await observeModel();
        item.hostMemoryAfterDispatch = { processRssBytes: process.memoryUsage().rss };
        const observedContext = item.modelAfterDispatch.contextWindowTokens;
        item.contextWindowVerification = observedContext === null || observedContext === undefined ? 'unknown'
          : observedContext === WINDOWS_SIX_MODEL.contextWindowTokens ? 'verified' : 'mismatch';
        if (item.contextWindowVerification === 'mismatch') throw failure('SIX_ACTUAL_MODEL_CONTEXT_MISMATCH');
        return response;
      } catch (error) { item.status = 'error'; item.errorCode = safeCode(error); throw error; }
      finally { item.durationMs = Math.ceil(performance.now() - before); }
    };
    const requestId = randomUUID();
    try { await runtime.reply({ conversationId, requestId, userMessageId: randomUUID(), provider: connection.providerId,
      model: selectedModel, message: fixture.message, permissionMode: 'full', runLimits: WINDOWS_SIX_LIMITS }); }
    finally {
      const assistant = (await conversations.readModelMessages(conversationId)).find(item => item.Id === requestId);
      if (assistant) assistants.push(assistant);
    }
    automaticEvidence = await collectAutomaticEvidence(runtime, tools, assistants, modelCalls, { workspace, projectId, conversationId });
    if (task.type === 'small-repair') {
      finalNodeTest = await independentNodeTest(workspace);
      finalNodeTest.testBytesPreserved = sha256(await readFile(join(workspace, 'pending-work.test.mjs'))) === sha256(PENDING_WORK_TEST);
    }
  } catch (error) { if (started === undefined) setupErrorCode = safeCode(error); else errorCode = safeCode(error); }
  finally {
    finished = performance.now(); clearTimeout(timer); globalThis.fetch = baseFetch;
    if (runtime) preCloseResourceSnapshot = await observeNativeResourceSnapshot(runtime.resources);
    // Closing only the owned runtime drains its index/worker; the caller owns the separate Ollama server.
    // 仅关闭本题自有运行时排空索引和 worker；独立 Ollama 服务归调用者管理，不卸载其他模型。
    try { await runtime?.close(); } catch (error) { closeErrorCode = safeCode(error); }
  }
  const answers = assistants.map(item => ({ content: item.Content, status: item.Status, error: item.Error ?? null,
    evidence: item.EvidenceReferences ?? [], modelSteps: item.ModelSteps ?? [], contextMetrics: item.ContextMetrics ?? null }));
  const formalToolActivities = assistants.flatMap(item => item.ToolActivities ?? []);
  const verification = verifySixTask(task, { answers, observations, automaticEvidence, finalNodeTest, memoryScopeCheck: memoryReceipt?.scopeCheck });
  const result = { adapterVersion: WINDOWS_SIX_ADAPTER_VERSION, scoringVersion: WINDOWS_SIX_SCORING_VERSION,
    taskId: task.id, type: task.type, plannedIndexState: task.indexState, fixture: fixture.provenance,
    executionStatus: setupErrorCode ? 'setup-error' : errorCode ? 'error' : closeErrorCode ? 'close-error'
      : assistants.at(-1)?.Status === 'completed' ? 'completed' : 'interrupted',
    setupErrorCode, errorCode, closeErrorCode, criticalFailure, setupMs: Math.ceil((started ?? performance.now()) - setupStarted),
    durationMs: started === undefined ? 0 : Math.ceil(finished - started),
    firstEffectiveActionMs, firstEvidenceResultMs, indexAtStart, indexAtFirstAction, indexBeforeChange, actionAdmission, corpusCoverage, memoryReceipt,
    modelCalls, retrievalCalls, observations, formalToolActivities, automaticEvidence, answers, finalNodeTest, verification,
    documentEmbedding: { policy: 'cache-reuse-with-bounded-real-increments',
      documentLimit: options.documentEmbeddingLimit ?? WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT,
      calls: documentEmbeddingCalls, admittedDocuments: documentEmbeddingCalls.filter(call => call.status !== 'budget-exhausted')
        .reduce((sum, call) => sum + call.requestedDocuments, 0),
      completedVectors: documentEmbeddingCalls.reduce((sum, call) => sum + (call.completedVectors ?? 0), 0),
      budgetExhausted: documentEmbeddingCalls.some(call => call.status === 'budget-exhausted') },
    callAccounting: { modelRequests: modelCalls.length, failedModelRequests: modelCalls.filter(item => item.status !== 'completed').length,
      formalToolAttempts: formalToolActivities.length, observedDispatches: observations.length,
      observedCompleted: observations.filter(item => item.status === 'completed').length,
      observedFailed: observations.filter(item => item.status !== 'completed').length,
      unknownUsageCalls: modelCalls.filter(item => item.usage.status === 'unknown').length },
    productTaskCompletion: assistants.at(-1)?.TaskCompletion ?? null,
    resourceObservation: { embedding: indexAtStart?.embedding ?? null, acquisitions: resourceAcquisitions,
      beforeClose: preCloseResourceSnapshot, callerMustSampleProcessTree: true },
    retainedFixturePath: taskRoot, noUserDataUsed: true, realNativeModelToolWire: true };
  if (prepared) result.preparedRetry = { policy: 'trusted-prepared-zero-effects-v1', priorResultHash: prepared.priorResultHash,
    workManifestHash: prepared.workManifestHash, workFileCount: prepared.workFileCount, reusedPreparedData: true,
    documentEmbeddingAttempts: documentEmbeddingCalls.length, overallIndexStatus: indexAtStart?.job?.status ?? null,
    priorCost: { modelCalls: prepared.prior.modelCalls, setupMs: prepared.prior.setupMs, durationMs: prepared.prior.durationMs,
      errorCode: prepared.prior.setupErrorCode ?? prepared.prior.errorCode, assistantErrors: prepared.priorAssistantErrors },
    emptyFailureHistory: prepared.emptyFailureHistory,
    historicalCostAccounting: 'Formal history preserves failure IDs and unknown usage; use original run artifacts once, never add these to current-run totals.',
    priorResultPreserved: sha256(await readFile(join(taskRoot, 'result.json'))) === prepared.priorResultHash };
  await writeFile(join(resultRoot, 'result.json'), JSON.stringify(result, null, 2));
  return result;
}

export async function runWindowsSix(options) {
  const endpoint = new URL(options.endpoint);
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(endpoint.hostname) || endpoint.protocol !== 'http:')
    throw failure('SIX_LOCAL_MODEL_ENDPOINT_REQUIRED');
  if (!options.output || !within(join(REPOSITORY_ROOT, 'artifacts'), options.output)) throw failure('SIX_OUTPUT_OUTSIDE_ARTIFACTS');
  if (!options.sourceRoot || !options.cacheRoot) throw failure('SIX_FROZEN_SOURCE_AND_CACHE_REQUIRED');
  const documentLimit = options.documentEmbeddingLimit ?? WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT;
  if (!Number.isSafeInteger(documentLimit) || documentLimit < 1 || documentLimit > 100000)
    throw failure('SIX_INVALID_DOCUMENT_EMBEDDING_LIMIT');
  await mkdir(options.output, { recursive: false });
  const plan = windowsSixPlan();
  await writeFile(join(options.output, 'plan-freeze.json'), JSON.stringify({ ...plan,
    sourceRoot: resolve(options.sourceRoot), cacheRoot: resolve(options.cacheRoot), sourceManifestHash: options.sourceManifestHash ?? null,
    documentEmbeddingLimit: options.documentEmbeddingLimit ?? WINDOWS_SIX_EMBEDDING_DOCUMENT_LIMIT,
    ...(options.preparedRoot ? { preparedRoot: resolve(options.preparedRoot), retryPolicy: 'trusted-prepared-zero-effects-v1' } : {}),
    startTime: new Date().toISOString() }, null, 2), { flag: 'wx' });
  const selectedIds = options.taskIds ?? WINDOWS_SIX_TASKS.map(task => task.id);
  if (options.preparedRoot && selectedIds.length !== 1) throw failure('SIX_PREPARED_REQUIRES_SINGLE_TASK');
  if (!selectedIds.length || new Set(selectedIds).size !== selectedIds.length || selectedIds.some(id => !WINDOWS_SIX_TASKS.some(task => task.id === id)))
    throw failure('SIX_INVALID_TASK_SELECTION');
  const product = await loadSixProduct(options.sourceRoot), runs = [];
  let stoppedAfter;
  for (const id of selectedIds) {
    options.onProgress?.({ taskId: id, state: 'starting' });
    let result;
    try { result = await runSixTask(product, WINDOWS_SIX_TASKS.find(task => task.id === id), options); }
    catch (error) {
      // Fixture preparation can fail before a runtime exists; retain an explicit zero-call receipt instead of losing the task.
      // 夹具准备可能在运行时创建前失败；保存零调用回执，不能让整题从成本和失败记录中消失。
      result = { adapterVersion: WINDOWS_SIX_ADAPTER_VERSION, scoringVersion: WINDOWS_SIX_SCORING_VERSION,
        taskId: id, executionStatus: 'setup-error', setupErrorCode: safeCode(error),
        modelCalls: [], observations: [], retrievalCalls: [], setupMs: null, durationMs: 0,
        verification: { mechanicalPassed: false, semanticReview: 'not-run', success: false } };
      await mkdir(join(options.output, id), { recursive: true });
      await writeFile(join(options.output, id, 'result.json'), JSON.stringify(result, null, 2));
    }
    runs.push(result); options.onProgress?.({ taskId: id, state: result.executionStatus,
      mechanicalPassed: result.verification.mechanicalPassed, semanticReview: result.verification.semanticReview });
    if (result.criticalFailure || result.closeErrorCode) { stoppedAfter = id; break; }
  }
  const result = { fixtureVersion: plan.fixtureVersion, fixtureHash: plan.fixtureHash,
    adapterVersion: WINDOWS_SIX_ADAPTER_VERSION, scoringVersion: WINDOWS_SIX_SCORING_VERSION, model: plan.model, limits: plan.limits,
    createdAt: new Date().toISOString(), selectedIds, runs, stoppedAfter,
    summary: { completed: runs.filter(item => item.executionStatus === 'completed').length,
      mechanicallyPassed: runs.filter(item => item.verification.mechanicalPassed).length,
      semanticallyPending: runs.filter(item => item.verification.semanticReview === 'pending').length,
      taskSuccessCount: runs.filter(item => item.verification.success === true && item.executionStatus === 'completed').length,
      unexecuted: selectedIds.slice(runs.length), notPublicLeaderboardScore: true } };
  await writeFile(join(options.output, 'results.json'), JSON.stringify(result, null, 2), { flag: 'wx' });
  return result;
}

async function main(args) {
  if (args.length === 0) { process.stdout.write(JSON.stringify(windowsSixPlan(), null, 2) + '\n'); return; }
  const values = {};
  const allowed = new Set(['--source-root', '--cache-root', '--output', '--endpoint', '--tool-host', '--embedding-root', '--task-ids', '--source-manifest-hash', '--model', '--prepared-root', '--document-embedding-limit']);
  for (let index = 0; index < args.length; index += 2) {
    if (!allowed.has(args[index]) || !args[index + 1] || args[index + 1].startsWith('--') || Object.hasOwn(values, args[index]))
      throw failure('SIX_INVALID_ARGUMENT');
    values[args[index]] = args[index + 1];
  }
  const result = await runWindowsSix({ sourceRoot: values['--source-root'], cacheRoot: values['--cache-root'],
    output: values['--output'], endpoint: values['--endpoint'], toolHost: values['--tool-host'], embeddingRoot: values['--embedding-root'],
    sourceManifestHash: values['--source-manifest-hash'], taskIds: values['--task-ids']?.split(','), model: values['--model'],
    preparedRoot: values['--prepared-root'],
    documentEmbeddingLimit: values['--document-embedding-limit'] === undefined ? undefined : Number(values['--document-embedding-limit']),
    onProgress: progress => process.stderr.write(JSON.stringify(progress) + '\n') });
  process.stdout.write(JSON.stringify(result.summary, null, 2) + '\n');
  if (result.stoppedAfter) process.exitCode = 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  main(process.argv.slice(2)).catch(error => { process.stderr.write(JSON.stringify({ errorCode: safeCode(error) }) + '\n'); process.exitCode = 1; });
