const MIB = 1024 * 1024;
const DEFAULT_LIMITS = Object.freeze({ maxBatchDocuments: 512, maxPendingRequests: 128, maxRequestBytes: 8 * MIB,
  maxQueuedBytes: 64 * MIB, maxQueuedEstimatedTokens: 8 * MIB, maxBatchInputTokens: 262_144 });

export function inferenceRequestLimits(options = {}) {
  const limits = { ...DEFAULT_LIMITS, ...options };
  const bounds = { maxBatchDocuments: [1, 4096], maxPendingRequests: [1, 1024], maxRequestBytes: [1024, 64 * MIB],
    maxQueuedBytes: [1024, 256 * MIB], maxQueuedEstimatedTokens: [512, 64 * MIB], maxBatchInputTokens: [512, 2_097_152] };
  for (const [name, [minimum, maximum]] of Object.entries(bounds))
    if (!Number.isSafeInteger(limits[name]) || limits[name] < minimum || limits[name] > maximum)
      throw new TypeError(`Invalid inference input limit: ${name}.`);
  if (limits.maxQueuedBytes < limits.maxRequestBytes) throw new TypeError('Queued input capacity cannot be smaller than one request.');
  return Object.freeze(limits);
}

export function inferenceInputBytes(inputs) {
  return inputs.reduce((sum, input) => sum + Buffer.byteLength(typeof input === 'string' ? input : input.text, 'utf8') +
    (typeof input === 'string' ? 0 : Buffer.byteLength(input.context ?? '', 'utf8')) + 64, 0);
}

/** Tickets cover native work after caller cancellation; release only a settled or exited worker's tickets.
 * 调用方取消后仍计入原生工作 ticket；仅在 worker 确认完成或退出后释放，防止取消绕过背压。 */
export class InferenceAdmission {
  #limits; #effectiveQueuedBytes; #tickets = new Map(); #sequence = 0; #bytes = 0; #estimatedTokens = 0;
  constructor(options) { this.#limits = inferenceRequestLimits(options); this.#effectiveQueuedBytes = this.#limits.maxQueuedBytes; }
  get limits() { return this.#limits; }
  setResourceBudget(memoryBytes) {
    if (Number.isFinite(memoryBytes) && memoryBytes > 0)
      this.#effectiveQueuedBytes = Math.min(this.#limits.maxQueuedBytes, Math.max(this.#limits.maxRequestBytes, Math.floor(memoryBytes / 8)));
  }
  reserve(inputs, repeatedQuery = '') {
    const queryBytes = Buffer.byteLength(repeatedQuery, 'utf8');
    const inputBytes = inferenceInputBytes(inputs) + queryBytes, estimatedTokens = inputBytes + queryBytes * Math.max(0, inputs.length - 1);
    if (inputs.length > this.#limits.maxBatchDocuments || inputBytes > this.#limits.maxRequestBytes ||
        this.#tickets.size >= this.#limits.maxPendingRequests || this.#bytes + inputBytes > this.#effectiveQueuedBytes ||
        this.#estimatedTokens + estimatedTokens > this.#limits.maxQueuedEstimatedTokens)
      throw Object.assign(new Error('Inference input budget is full.'), { code: 'INFERENCE_INPUT_BACKPRESSURE' });
    const ticket = ++this.#sequence;
    this.#tickets.set(ticket, { inputBytes, estimatedTokens }); this.#bytes += inputBytes; this.#estimatedTokens += estimatedTokens;
    return ticket;
  }
  release(ticket) {
    const entry = this.#tickets.get(ticket);
    if (!entry) return;
    this.#tickets.delete(ticket); this.#bytes -= entry.inputBytes; this.#estimatedTokens -= entry.estimatedTokens;
  }
  clear() { this.#tickets.clear(); this.#bytes = 0; this.#estimatedTokens = 0; }
  status() { return { activeRequests: this.#tickets.size, queuedInputBytes: this.#bytes, queuedEstimatedTokens: this.#estimatedTokens,
    limits: { ...this.#limits, effectiveQueuedBytes: this.#effectiveQueuedBytes } }; }
}
