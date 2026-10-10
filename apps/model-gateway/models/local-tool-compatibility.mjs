import { isIP } from 'node:net';

const MAX_DECLARED_MINIMUM_REPETITIONS = 32;
const MAX_REJECTION_BYTES = 65_536;
const MAX_REJECTION_READ_MS = 2000;
const MAX_SCHEMA_DEPTH = 64;
const MAX_SCHEMA_NODES = 32_768;
const schemaMaps = new Set(['properties', 'patternProperties', '$defs', 'definitions', 'dependentSchemas']);
const schemaChildren = new Set(['items', 'additionalItems', 'additionalProperties', 'contains', 'propertyNames',
  'not', 'if', 'then', 'else', 'unevaluatedProperties', 'unevaluatedItems', 'contentSchema']);
const schemaLists = new Set(['allOf', 'anyOf', 'oneOf', 'prefixItems']);
const structuralConstraints = new Set(['minLength', 'maxLength', 'minItems', 'maxItems', 'pattern', 'format',
  'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum', 'multipleOf', 'minContains', 'maxContains',
  'minProperties', 'maxProperties', 'uniqueItems']);

function localLlamaEndpoint(connection, localModel) {
  let endpoint;
  try { endpoint = new URL(connection?.baseUrl); } catch { return false; }
  const host = endpoint.hostname.replace(/^\[|\]$/gu, '').toLowerCase();
  const isLoopback = host === 'localhost' || host === '::1' || isIP(host) === 4 && host.split('.')[0] === '127';
  return isLoopback && ['http:', 'https:'].includes(endpoint.protocol) && !endpoint.username && !endpoint.password &&
    !endpoint.search && !endpoint.hash && localModel?.backend === 'llama.cpp' &&
    localModel.source === 'llama-cpp-props' && localModel.observationOnly === true &&
    localModel.endpointOrigin === endpoint.origin;
}

function omitConstraint(key, value, mode) {
  if (mode === 'structural') return structuralConstraints.has(key);
  // Finite upper bounds expand grammar rules; executor validation remains authoritative.
  // 有限上界会展开 grammar 规则；执行器仍以原始 schema 验证参数。
  return key === 'maxLength' || key === 'maxItems' ||
    (key === 'minLength' || key === 'minItems') && Number.isFinite(value) && value > MAX_DECLARED_MINIMUM_REPETITIONS;
}

function projectSchema(schema, mode, state, depth = 0) {
  if (!schema || typeof schema !== 'object' || Array.isArray(schema)) return schema;
  if (depth > MAX_SCHEMA_DEPTH || ++state.nodes > MAX_SCHEMA_NODES)
    throw new RangeError('Local tool declaration schema exceeds its projection bound.');
  const projected = {};
  for (const [key, value] of Object.entries(schema)) {
    if (omitConstraint(key, value, mode)) {
      state.omittedConstraintCount++;
    } else if (schemaMaps.has(key) && value && typeof value === 'object' && !Array.isArray(value)) {
      projected[key] = Object.fromEntries(Object.entries(value).map(([name, child]) =>
        [name, projectSchema(child, mode, state, depth + 1)]));
    } else if ((schemaLists.has(key) || key === 'items') && Array.isArray(value)) {
      projected[key] = value.map(child => projectSchema(child, mode, state, depth + 1));
    } else if (schemaChildren.has(key)) {
      projected[key] = projectSchema(value, mode, state, depth + 1);
    } else {
      // Enum/default/example objects are business values, not nested schemas.
      // enum、default、example 中的对象是业务值，不能按嵌套 schema 删字段。
      Object.defineProperty(projected, key, { value, enumerable: true, configurable: true, writable: true });
    }
  }
  return projected;
}

/** Project model declarations only; callers must retain the original catalog for decoding and execution.
 * 只投影模型声明；调用方必须保留原始目录用于调用解码与执行验证。 */
export function projectLocalToolCatalog(catalog, { connection, localModel, mode = 'bounded' } = {}) {
  if (!['bounded', 'structural'].includes(mode)) throw new TypeError('Unknown local tool declaration projection mode.');
  if (!Array.isArray(catalog)) throw new TypeError('Local tool declaration projection requires a catalog.');
  const unchanged = { catalog, applied: false, mode, omittedConstraintCount: 0 };
  if (!catalog.length || !localLlamaEndpoint(connection, localModel)) return unchanged;
  const state = { nodes: 0, omittedConstraintCount: 0 };
  const projectedCatalog = catalog.map(descriptor => ({ ...descriptor,
    inputSchema: projectSchema(descriptor.inputSchema, mode, state) }));
  return state.omittedConstraintCount ? { catalog: projectedCatalog, applied: true, mode,
    omittedConstraintCount: state.omittedConstraintCount } : unchanged;
}

/** Recognize only an explicit HTTP grammar rejection; never expose reflected upstream messages.
 * 仅识别明确的 HTTP grammar 拒绝，不公开上游可能回显私有输入的错误正文。 */
export function localToolGrammarRejection(body, { status } = {}) {
  if (status !== 400) return null;
  const error = body?.error;
  const message = typeof error === 'string' ? error : typeof error?.message === 'string' ? error.message
    : typeof body?.message === 'string' ? body.message : '';
  if (message.length > MAX_REJECTION_BYTES || !/\b(?:failed to (?:parse|initialize)[^\n]{0,120}grammar|error parsing grammar|number of (?:rules|repetitions)[^\n]{0,240}exceeds sane defaults)\b/iu.test(message))
    return null;
  return { kind: 'tool-grammar', code: 'MODEL_LOCAL_TOOL_GRAMMAR_REJECTED' };
}

export async function readLocalToolGrammarRejection(response) {
  if (response?.status !== 400 || !response.body || Number(response.headers?.get('content-length')) > MAX_REJECTION_BYTES)
    return null;
  const reader = response.body.getReader(), chunks = [];
  let bytes = 0, complete = false, timedOut = false;
  const timer = setTimeout(() => { timedOut = true; reader.cancel().catch(() => {}); }, MAX_REJECTION_READ_MS);
  try {
    for (;;) {
      const item = await reader.read();
      if (item.done) { complete = !timedOut; break; }
      bytes += item.value.byteLength;
      if (bytes > MAX_REJECTION_BYTES) break;
      chunks.push(Buffer.from(item.value));
    }
    if (!complete) return null;
    let body;
    try { body = JSON.parse(Buffer.concat(chunks, bytes).toString('utf8')); } catch { return null; }
    return localToolGrammarRejection(body, { status: response.status });
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
    if (!complete) await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

/** One rejected model step may retry with simpler declarations; no generated output or tool action may be replayed.
 * 单个被拒绝的模型步骤可用更简单声明重试一次，已生成输出或已执行工具动作不能重放。 */
export function planLocalToolGrammarRecovery({ rejection, connection, localModel, catalog,
  recoveryAttempts = 0, hasVisibleOutput = false, executedToolCalls = 0 } = {}) {
  if (rejection?.kind !== 'tool-grammar' || rejection.code !== 'MODEL_LOCAL_TOOL_GRAMMAR_REJECTED' ||
    recoveryAttempts !== 0 || hasVisibleOutput || executedToolCalls !== 0 || !Array.isArray(catalog)) return null;
  const projection = projectLocalToolCatalog(catalog, { connection, localModel, mode: 'structural' });
  return projection.applied ? { ...projection, recoveryAttempts: 1, code: 'MODEL_LOCAL_TOOL_GRAMMAR_RECOVERY' } : null;
}
