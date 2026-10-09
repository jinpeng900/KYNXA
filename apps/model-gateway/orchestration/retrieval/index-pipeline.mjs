/** Bounded lookahead overlaps source preparation with inference without detaching accepted work.
 * 有界预取让来源准备与推理重叠；取消和消费失败仍等待已接纳工作真实结束。
 */
export async function* prepareSourcePipeline(sources, prepare, { concurrency = 2, signal } = {}) {
  if (!Number.isSafeInteger(concurrency) || concurrency < 1 || concurrency > 4)
    throw new TypeError('Invalid source preparation concurrency.');
  const pending = new Map();
  let next = 0;
  const schedule = () => {
    while (next < sources.length && pending.size < concurrency && !signal?.aborted) {
      const position = next++, input = sources[position];
      const operation = Promise.resolve().then(() => {
        signal?.throwIfAborted();
        return prepare(input);
      }).then(value => ({ input, value }), error => ({ input, error }));
      pending.set(position, operation);
    }
  };
  try {
    schedule();
    for (let position = 0; position < sources.length; position++) {
      signal?.throwIfAborted();
      const result = await pending.get(position);
      pending.delete(position);
      schedule();
      signal?.throwIfAborted();
      yield result;
    }
  } finally {
    // All rejections are observed and all native preparation settles before releasing the owner's lifetime.
    // 所有错误均被观察；原生准备任务排空前不能释放所属生命周期。
    await Promise.allSettled([...pending.values()]);
  }
}

/** One publication may overlap derivation; its rejection is observed immediately and propagated at the next boundary.
 * 一次发布可与派生重叠；失败立即被观察，在下一边界传播，不能丢失已提交的回执。
 */
export class IndexPublicationStage {
  constructor() { this.pending = null; }
  async drain() {
    const pending = this.pending;
    if (!pending) return;
    this.pending = null;
    const receipt = await pending;
    if (receipt.error) throw receipt.error;
    return receipt.value;
  }
  async submit(operation) {
    await this.drain();
    this.pending = Promise.resolve().then(operation).then(value => ({ value }), error => ({ error }));
  }
}
