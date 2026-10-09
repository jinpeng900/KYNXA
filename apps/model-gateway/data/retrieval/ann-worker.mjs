import { createRequire } from 'node:module';

const { Index } = createRequire(import.meta.url)('usearch');
const indexes = new Map();
let observedPeakRssBytes = 0;
const keyPattern = /^[a-f0-9]{64}$/u;
const filenamePattern = /^[a-f0-9]{64}\.usearch(?:\.[a-f0-9-]{36}\.tmp)?$/u;

function ownedIndex(key) {
  if (!keyPattern.test(key)) throw Object.assign(new Error('Invalid ANN shard identity.'), { code: 'INVALID_RETRIEVAL_ANN' });
  const index = indexes.get(key);
  if (!index) throw Object.assign(new Error('ANN shard is unavailable.'), { code: 'RETRIEVAL_ANN_SHARD_MISSING' });
  return index;
}

/** The owned helper uses ASCII leaf names inside its Unicode working directory.
 * 独立且受管理的计算进程在 Unicode 工作目录中仅向原生库传 ASCII 文件名，兼容 Windows 路径。 */
process.on('message', ({ id, method, input }) => {
  try {
    if (!Number.isSafeInteger(id) || !input || typeof input !== 'object') throw new Error('Invalid ANN request.');
    const { key } = input;
    let result;
    if (method === 'create') {
      if (!keyPattern.test(key)) throw new Error('Invalid ANN shard identity.');
      if (!indexes.has(key) && indexes.size >= 16) throw new Error('Too many owned ANN shards.');
      if (!Number.isSafeInteger(input.dimensions) || input.dimensions < 1 || input.dimensions > 4096 ||
          !Number.isSafeInteger(input.connectivity) || input.connectivity < 4 || input.connectivity > 64 ||
          !Number.isSafeInteger(input.expansionAdd) || input.expansionAdd < 16 || input.expansionAdd > 2048 ||
          !Number.isSafeInteger(input.expansionSearch) || input.expansionSearch < 16 || input.expansionSearch > 2048)
        throw new Error('Invalid ANN graph dimensions or configuration.');
      indexes.set(key, new Index({ dimensions: input.dimensions, metric: 'cos', quantization: 'f32',
        connectivity: input.connectivity, expansion_add: input.expansionAdd, expansion_search: input.expansionSearch }));
      result = { created: true };
    } else if (method === 'drop') { indexes.delete(key); result = { removed: true }; }
    else if (method === 'close') { indexes.clear(); result = { closed: true }; }
    else {
      const index = ownedIndex(key);
      if (method === 'add') {
        if (!(input.keys instanceof BigUint64Array) || input.keys.length > 256 || !Array.isArray(input.vectors) || input.vectors.length !== input.keys.length ||
            input.vectors.some(vector => !(vector instanceof Float32Array) || vector.length !== index.dimensions() || !vector.every(Number.isFinite)))
          throw new Error('Invalid ANN vector batch.');
        const threads = Number.isSafeInteger(input.threads) ? Math.max(1, Math.min(32, input.threads)) : 1;
        index.add(input.keys, input.vectors, threads); result = { count: index.size() };
      } else if (method === 'remove') {
        if (!(input.keys instanceof BigUint64Array) || input.keys.length > 1000000) throw new Error('Invalid ANN removal batch.');
        index.remove(input.keys); result = { count: index.size() };
      }
      else if (method === 'search') {
        if (!(input.query instanceof Float32Array) || input.query.length !== index.dimensions() || !input.query.every(Number.isFinite) ||
            !Number.isSafeInteger(input.limit) || input.limit < 1 || input.limit > 160) throw new Error('Invalid ANN query.');
        const threads = Number.isSafeInteger(input.threads) ? Math.max(1, Math.min(32, input.threads)) : 1;
        const matches = index.search(input.query, input.limit, threads);
        result = { keys: matches.keys, distances: matches.distances };
      } else if (method === 'load' || method === 'save') {
        if (!filenamePattern.test(input.filename)) throw new Error('Invalid ANN cache filename.');
        index[method](input.filename);
        result = { count: index.size(), dimensions: index.dimensions() };
      } else throw new Error('Unknown ANN operation.');
    }
    const rssBytes = process.memoryUsage().rss;
    observedPeakRssBytes = Math.max(observedPeakRssBytes, rssBytes);
    process.send?.({ id, result, memory: { rssBytes, observedPeakRssBytes } }, () => { if (method === 'close') process.disconnect(); });
  } catch (error) {
    process.send?.({ id, error: { message: error.message, code: error.code ?? 'RETRIEVAL_ANN_FAILED' } });
  }
});

// Orphaned helpers cannot retain an index or become a background service.
// 父进程失联后释放计算进程，不留下孤立索引进程或后台服务。
process.on('disconnect', () => process.exit(0));
