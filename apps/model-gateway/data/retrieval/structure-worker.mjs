import { parentPort } from 'node:worker_threads';
import { codeLanguageForSource, parseCodeStructure } from './code-structure.mjs';
import { parseDocumentStructure } from './document-structure.mjs';
import { chunkStructuredSource, STRUCTURED_CHUNKER_VERSION, STRUCTURED_EMBEDDING_TEXT_VERSION } from './retrieval-text.mjs';
import { validateSource } from './retrieval-contracts.mjs';

const OTHER_CODE_EXTENSIONS = /\.(?:py|rs|go|java|cpp|c|h|ps1|sql)$/iu;

function requestSignal(cancelBuffer) {
  const flag = new Int32Array(cancelBuffer);
  return { get aborted() { return Atomics.load(flag, 0) !== 0; },
    throwIfAborted() {
      if (this.aborted) throw Object.assign(new Error('Source parsing cancelled. / 来源解析已取消。'), { name: 'AbortError', code: 'ABORT_ERR' });
    } };
}

/** Only supplied text is parsed; the worker never opens a mounted path or launches an interpreter.
 * 只解析调用方提供的正文，不打开挂载路径、不启动命令解释器。 */
async function parse(message) {
  const signal = requestSignal(message.cancelBuffer), started = performance.now();
  signal.throwIfAborted();
  const source = validateSource(message.source);
  const language = codeLanguageForSource(source);
  const unsupportedCode = !['message', 'memory'].includes(source.sourceType) &&
    OTHER_CODE_EXTENSIONS.test(source.locator.relativePath ?? source.locator.path ?? source.title);
  const parsed = language ? await parseCodeStructure(source, { signal }) : unsupportedCode
    ? { domain: 'code', language: null, parserVersion: 'unsupported-code-v1', parseStatus: 'unavailable',
      diagnosticCodes: ['UNSUPPORTED_CODE_LANGUAGE'], units: [] }
    : parseDocumentStructure(source, { checkCancelled: () => signal.throwIfAborted() });
  signal.throwIfAborted();
  const { units, ...structure } = parsed;
  const preparedSource = { ...source, structure, parserVersion: structure.parserVersion,
    chunkerVersion: STRUCTURED_CHUNKER_VERSION, embeddingInputVersion: STRUCTURED_EMBEDDING_TEXT_VERSION };
  const chunks = chunkStructuredSource(preparedSource, { ...structure, units }, { maxChars: message.maxChars,
    checkCancelled: () => signal.throwIfAborted() });
  signal.throwIfAborted();
  return { structure, parserVersion: structure.parserVersion, chunkerVersion: STRUCTURED_CHUNKER_VERSION,
    embeddingInputVersion: STRUCTURED_EMBEDDING_TEXT_VERSION, chunks, parsingMs: performance.now() - started };
}

let queue = Promise.resolve();
parentPort.on('message', message => {
  // One parser sequence avoids concurrent language changes and bounds retained syntax trees.
  // 单一解析队列避免并发切换语言，并限制语法树驻留数量。
  queue = queue.catch(() => {}).then(async () => {
    try { parentPort.postMessage({ id: message.id, result: await parse(message) }); }
    catch (error) { parentPort.postMessage({ id: message.id, error: { name: error.name, code: error.code ?? 'STRUCTURE_PARSE_FAILED',
      message: error.name === 'AbortError' ? 'Source parsing cancelled. / 来源解析已取消。' : 'Source parsing failed. / 来源解析失败。' } }); }
  });
});
