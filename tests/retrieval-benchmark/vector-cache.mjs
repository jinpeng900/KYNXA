import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const CACHE_SCHEMA_VERSION = 1;
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

/** Cache only exact, verifiable public benchmark inference inputs and Float32 outputs.
 * 仅缓存可核验的公开评测推理输入和 Float32 结果；任何来源、分块或模型变化都会改变缓存身份。
 */
export class BenchmarkVectorCache {
  constructor({ root, modelIdentity, inputVersion, dimensions }) {
    assert.equal(typeof inputVersion, 'string');
    assert.ok(Number.isSafeInteger(dimensions) && dimensions > 0);
    this.root = root;
    this.identity = { schemaVersion: CACHE_SCHEMA_VERSION, modelIdentity, inputVersion, dimensions,
      encoding: 'float32-little-endian' };
    this.stats = { hitBatches: 0, hitVectors: 0, computedBatches: 0, computedVectors: 0, invalidBatches: 0 };
  }

  async getOrCompute(records, compute) {
    const inputs = records.map(record => ({ ...record, inputSha256: sha256(record.input) }));
    const key = sha256(JSON.stringify({ identity: this.identity, inputs }));
    const directory = join(this.root, key.slice(0, 2));
    const manifestPath = join(directory, key + '.json'), binaryPath = join(directory, key + '.bin');
    const expectedBytes = records.length * this.identity.dimensions * Float32Array.BYTES_PER_ELEMENT;
    try {
      const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
      const bytes = await readFile(binaryPath);
      assert.deepEqual(manifest.identity, this.identity);
      assert.equal(manifest.key, key);
      assert.deepEqual(manifest.inputs, inputs);
      assert.equal(bytes.length, expectedBytes);
      assert.equal(sha256(bytes), manifest.vectorSha256);
      const vectors = Array.from({ length: records.length }, (_, row) => Array.from({ length: this.identity.dimensions },
        (_, column) => bytes.readFloatLE((row * this.identity.dimensions + column) * 4)));
      validateVectors(vectors, records.length, this.identity.dimensions);
      this.stats.hitBatches++;
      this.stats.hitVectors += vectors.length;
      return vectors;
    } catch (error) {
      if (error.code !== 'ENOENT') this.stats.invalidBatches++;
    }
    const vectors = await compute(records.map(record => record.input));
    validateVectors(vectors, records.length, this.identity.dimensions);
    const bytes = Buffer.allocUnsafe(expectedBytes);
    for (let row = 0; row < vectors.length; row++) for (let column = 0; column < vectors[row].length; column++)
      bytes.writeFloatLE(vectors[row][column], (row * this.identity.dimensions + column) * 4);
    const manifest = { identity: this.identity, key, inputs, vectorCount: vectors.length,
      vectorBytes: bytes.length, vectorSha256: sha256(bytes), createdAt: new Date().toISOString() };
    await mkdir(directory, { recursive: true });
    const nonce = '.' + randomUUID() + '.tmp';
    await writeFile(binaryPath + nonce, bytes, { flag: 'wx' });
    await writeFile(manifestPath + nonce, JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx' });
    // Publish the manifest last. A crash can never make incomplete bytes look reusable.
    // 清单最后发布；崩溃不能让不完整向量成为可复用的成功缓存。
    await rename(binaryPath + nonce, binaryPath);
    await rename(manifestPath + nonce, manifestPath);
    this.stats.computedBatches++;
    this.stats.computedVectors += vectors.length;
    // Fresh and cached passes use the same Float32 precision as the real SQLite vector index.
    // 新算与缓存两条路径统一到实际 SQLite 向量索引所用的 Float32 精度。
    return vectors.map(vector => Array.from(new Float32Array(vector)));
  }
}

function validateVectors(vectors, count, dimensions) {
  assert.equal(vectors.length, count, 'Every original input must receive one real vector.');
  for (const vector of vectors) {
    assert.equal(vector.length, dimensions);
    assert.ok(vector.every(Number.isFinite));
    assert.ok(vector.some(value => value !== 0), 'Zero vectors are not usable inference receipts.');
  }
}
