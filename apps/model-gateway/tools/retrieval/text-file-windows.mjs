import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import { inspectLocalPath, toolFailure } from '../../platform/tool-paths.mjs';

export const TEXT_WINDOW_VERSION = 'text-file-window-v1';
export const TEXT_WINDOW_CHARACTERS = 128 * 1024;
const READ_BYTES = 64 * 1024;
const hash = value => createHash('sha256').update(value).digest('hex');
const metadata = info => ({ sizeBytes: info.size, mtimeMs: info.mtimeMs, ctimeMs: info.ctimeMs, device: info.dev, inode: info.ino });
const same = (left, right) => ['sizeBytes', 'mtimeMs', 'ctimeMs', 'device', 'inode'].every(key => left[key] === right[key]);
const stale = () => toolFailure('原始文本文件已变化，请重新扫描。', 'STALE_RETRIEVAL_SOURCE', 409);

/** Hash and describe the whole file while retaining only one bounded text window, never the complete body.
 * 流式校验并描述整个文件，内存仅保留一个有界文本窗口，不拼接完整大文件正文。
 */
export async function describeTextFileWindows(path, { maximumSourceBytes, signal } = {}) {
  signal?.throwIfAborted();
  const before = await inspectLocalPath(path);
  if (!before.isFile() || before.size > maximumSourceBytes) throw toolFailure('资料文件超过配置预算。', 'INVALID_RETRIEVAL_SOURCE', 413);
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (opened.nlink !== 1 || opened.ino !== before.ino || opened.dev !== before.dev) throw stale();
    const buffer = Buffer.allocUnsafe(READ_BYTES), rawHasher = createHash('sha256'), textHasher = createHash('sha256');
    let decoder, encoding, bytePosition = 0, nextWindowByte = 0, pending = '', characterPosition = 0, line = 1, textBytes = 0;
    const windows = [];
    const publish = text => {
      const bytes = Buffer.byteLength(text, encoding === 'utf-16le' ? 'utf16le' : 'utf8');
      const startLine = line; line += (text.match(/\n/gu) ?? []).length;
      windows.push({ version: TEXT_WINDOW_VERSION, encoding, offsetUnit: 'utf16-code-units',
        startOffset: characterPosition, endOffset: characterPosition + text.length, startLine, endLine: line,
        startByte: nextWindowByte, endByte: nextWindowByte + bytes, contentHash: hash(text), textBytes: Buffer.byteLength(text) });
      characterPosition += text.length; nextWindowByte += bytes;
    };
    const append = text => {
      if (text.includes('\0')) throw toolFailure('二进制文件不能作为文本索引。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400);
      textHasher.update(text); textBytes += Buffer.byteLength(text); pending += text;
      while (pending.length >= TEXT_WINDOW_CHARACTERS) {
        let end = TEXT_WINDOW_CHARACTERS;
        if (/[\uD800-\uDBFF]/u.test(pending[end - 1])) end--;
        publish(pending.slice(0, end)); pending = pending.slice(end);
      }
    };
    while (bytePosition < opened.size) {
      signal?.throwIfAborted();
      const read = await handle.read(buffer, 0, Math.min(buffer.length, opened.size - bytePosition), bytePosition);
      if (!read.bytesRead) throw stale();
      let bytes = buffer.subarray(0, read.bytesRead); rawHasher.update(bytes);
      if (!decoder) {
        encoding = bytes[0] === 0xff && bytes[1] === 0xfe ? 'utf-16le' : 'utf-8';
        const bom = encoding === 'utf-16le' ? 2 : bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf ? 3 : 0;
        nextWindowByte = bom; bytes = bytes.subarray(bom);
        decoder = new TextDecoder(encoding, { fatal: true, ignoreBOM: true });
      }
      try { append(decoder.decode(bytes, { stream: true })); }
      catch (error) { if (error.code) throw error; throw toolFailure('资料字符编码无效。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400); }
      bytePosition += read.bytesRead;
    }
    try { append(decoder?.decode() ?? ''); }
    catch { throw toolFailure('资料字符编码无效。', 'UNSUPPORTED_RETRIEVAL_SOURCE', 400); }
    if (pending.length) publish(pending);
    const after = await inspectLocalPath(path);
    signal?.throwIfAborted();
    if (!same(metadata(opened), metadata(after)) || nextWindowByte !== opened.size) throw stale();
    const rawContentHash = rawHasher.digest('hex'), contentHash = textHasher.digest('hex');
    return { contentHash, rawContentHash, textBytes, metadata: metadata(after), windows: windows.map(window => ({ ...window,
      rawContentHash, textContentHash: contentHash, totalCharacters: characterPosition, metadata: metadata(after) })) };
  } finally { await handle.close(); }
}

/** Read a verified byte window; source stat/version plus the window hash prevent stale offset reuse.
 * 回读已校验的字节窗口，以来源状态、版本和窗口哈希拒绝过期偏移，不重新拼接整个文件。
 */
export async function readTextFileWindow(path, window, { maximumSourceBytes, signal } = {}) {
  signal?.throwIfAborted();
  const before = await inspectLocalPath(path);
  if (!before.isFile() || before.size > maximumSourceBytes || !window?.metadata || !same(metadata(before), window.metadata)) throw stale();
  const length = window.endByte - window.startByte;
  if (window.version !== TEXT_WINDOW_VERSION || !['utf-8', 'utf-16le'].includes(window.encoding) ||
      !Number.isSafeInteger(window.startByte) || window.startByte < 0 || !Number.isSafeInteger(length) || length < 1 ||
      length > TEXT_WINDOW_CHARACTERS * 4 || window.endByte > before.size) throw toolFailure('来源文本窗口无效。', 'INVALID_RETRIEVAL_WINDOW', 400);
  const handle = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const opened = await handle.stat();
    if (opened.nlink !== 1 || !same(metadata(opened), window.metadata)) throw stale();
    const bytes = Buffer.allocUnsafe(length); let count = 0;
    while (count < length) {
      signal?.throwIfAborted();
      const read = await handle.read(bytes, count, length - count, window.startByte + count);
      if (!read.bytesRead) throw stale(); count += read.bytesRead;
    }
    let text;
    try { text = new TextDecoder(window.encoding, { fatal: true, ignoreBOM: true }).decode(bytes); }
    catch { throw stale(); }
    const after = await inspectLocalPath(path);
    signal?.throwIfAborted();
    if (!same(metadata(after), window.metadata) || text.length !== window.endOffset - window.startOffset || hash(text) !== window.contentHash) throw stale();
    return { text, contentHash: window.contentHash, metadata: metadata(after), textBytes: Buffer.byteLength(text), window };
  } finally { await handle.close(); }
}
