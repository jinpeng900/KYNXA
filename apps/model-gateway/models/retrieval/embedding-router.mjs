import { EmbeddingService } from './embedding-service.mjs';
import { resolveRetrievalModelProfile } from './model-registry.mjs';

function waitForRetirement(completion, signal) {
  const cancelled = () => Object.assign(new Error('Embedding request was cancelled. / 嵌入请求已取消。'),
    { code: 'EMBEDDING_CANCELLED', name: 'AbortError' });
  if (signal?.aborted) return Promise.reject(cancelled());
  if (!signal) return completion;
  return new Promise((resolve, reject) => {
    const abort = () => { signal.removeEventListener('abort', abort); reject(cancelled()); };
    signal.addEventListener('abort', abort, { once: true });
    completion.then(result => { signal.removeEventListener('abort', abort); resolve(result); },
      error => { signal.removeEventListener('abort', abort); reject(error); });
  });
}

/** Each selected profile owns its own session and vector identity; no profile is relabeled on fallback.
 * 每个配置拥有独立会话与向量身份，回退不能给另一配置的向量改标签。 */
export class EmbeddingRouter {
  constructor({ resourceService, factory = options => new EmbeddingService(options) } = {}) {
    this.resources = resourceService; this.factory = factory; this.instances = new Map(); this.retiring = new Map(); this.closed = false;
  }
  _instance(profileId = 'builtin-multilingual') {
    resolveRetrievalModelProfile('embedding', profileId);
    if (this.closed) throw Object.assign(new Error('Embedding router is closed. / 嵌入路由已关闭。'), { code: 'EMBEDDING_CLOSED' });
    if (!this.instances.has(profileId)) this.instances.set(profileId, this.factory({ resourceService: this.resources, profileId }));
    return this.instances.get(profileId);
  }
  status(profileId = 'builtin-multilingual') {
    try { return (this.retiring.get(profileId)?.instance ?? this._instance(profileId)).status(profileId); }
    catch (error) {
      if (error.code !== 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED') throw error;
      return this._instance().status(profileId);
    }
  }
  async _request(method, input, options) {
    const profileId = options.profileId ?? 'builtin-multilingual';
    const retiring = this.retiring.get(profileId);
    if (retiring) await waitForRetirement(retiring.completion, options.signal);
    return this._instance(profileId)[method](input, options);
  }
  fitDocuments(documents, options = {}) { return this._request('fitDocuments', documents, options); }
  embedDocuments(texts, options = {}) { return this._request('embedDocuments', texts, options); }
  embedQuery(query, options = {}) { return this._request('embedQuery', query, options); }

  // New calls wait for confirmed retirement, then create a lazy session with the same vector identity.
  // 新调用等待安全退役回执，再按同一向量身份创建按需会话；旧请求不会自动重放。
  async releaseIdleGpu({ signal } = {}) {
    if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
    const results = [], completions = [...this.retiring.values()].map(retirement => retirement.completion);
    for (const [profileId, instance] of this.instances) {
      const claim = instance.tryRetireIdleGpu?.({ signal }) ?? { retiring: false, reason: 'retirement-unsupported' };
      if (!claim.retiring) { results.push({ profileId, released: false, reason: claim.reason }); continue; }
      this.instances.delete(profileId);
      const retirement = { instance, completion: undefined };
      retirement.completion = claim.completion.then(() => {
        if (this.retiring.get(profileId) === retirement) this.retiring.delete(profileId);
        return { profileId, released: true, gpuMemoryBytes: claim.gpuMemoryBytes };
      });
      // Failed retirement remains a barrier; do not load a replacement while owned cleanup is uncertain.
      // 退役失败保留屏障，不能在本应用旧进程清理状态不确定时加载替代模型。
      this.retiring.set(profileId, retirement);
      completions.push(retirement.completion);
    }
    results.push(...await Promise.all(completions));
    return { released: results.some(result => result.released), results };
  }
  async close() {
    this.closed = true;
    await Promise.all([...this.instances.values()].map(instance => instance.close())
      .concat([...this.retiring.values()].map(retirement => retirement.completion)));
  }
}
