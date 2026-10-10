import { RerankerService } from './reranker-service.mjs';
import { resolveRetrievalModelProfile, unavailableProfileStatus } from './model-registry.mjs';

function waitForRetirement(completion, signal) {
  const cancelled = () => Object.assign(new Error('Reranking cancelled. / 重排请求已取消。'), { code: 'RERANK_CANCELLED', name: 'AbortError' });
  if (signal?.aborted) return Promise.reject(cancelled());
  if (!signal) return completion;
  return new Promise((resolve, reject) => {
    const abort = () => { signal.removeEventListener('abort', abort); reject(cancelled()); };
    signal.addEventListener('abort', abort, { once: true });
    completion.then(result => { signal.removeEventListener('abort', abort); resolve(result); },
      error => { signal.removeEventListener('abort', abort); reject(error); });
  });
}

/** Exact reranker profiles and device preferences own isolated sessions; policy changes wait for native drainage.
 * 精确重排配置与设备偏好拥有独立会话；策略变化等待原生排空，不重放旧请求。 */
export class RerankerRouter {
  constructor({ resourceService, factory = options => new RerankerService(options) } = {}) {
    this.resources = resourceService; this.factory = factory; this.instances = new Map(); this.preferences = new Map();
    this.retiring = new Map(); this.configuration = Promise.resolve(); this.closed = false;
  }
  _instance(profileId = 'builtin-multilingual-reranker', devicePreference = this.preferences.get(profileId) ?? 'auto') {
    resolveRetrievalModelProfile('reranker', profileId);
    if (this.closed) throw Object.assign(new Error('Reranker router is closed. / 重排路由已关闭。'), { code: 'RERANK_CLOSED' });
    if (!this.instances.has(profileId)) {
      this.instances.set(profileId, this.factory({ resourceService: this.resources, profileId, devicePreference }));
      this.preferences.set(profileId, devicePreference);
    }
    return this.instances.get(profileId);
  }
  status(profileId = 'builtin-multilingual-reranker', { devicePreference } = {}) {
    try {
      const retirement = this.retiring.get(profileId);
      const preference = retirement?.preference ?? this.preferences.get(profileId) ?? devicePreference ?? 'auto';
      const result = (retirement?.instance ?? this._instance(profileId, preference)).status(profileId);
      // A status read cannot mutate a CPU preference or reopen a draining model.
      // 状态读取不能修改 CPU 偏好，也不能重新打开仍在排空的模型。
      return { ...result, devicePreference: preference, executionIdentity: `${profileId}:${preference}`,
        ...(devicePreference && devicePreference !== preference ? { requestedDevicePreference: devicePreference,
          requiresReconfiguration: true } : {}) };
    } catch (error) {
      if (error.code !== 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED') throw error;
      return unavailableProfileStatus('reranker', profileId);
    }
  }
  _retire(profileId, instance) {
    this.instances.delete(profileId);
    const retirement = { instance, preference: this.preferences.get(profileId), completion: undefined };
    retirement.completion = Promise.resolve().then(() => instance.retire?.() ?? instance.close()).then(() => {
      if (this.retiring.get(profileId) === retirement) this.retiring.delete(profileId);
    });
    this.retiring.set(profileId, retirement);
    return retirement.completion;
  }
  configure({ profileId = 'builtin-multilingual-reranker', devicePreference = 'auto', retireOtherProfiles = false, signal } = {}, dispatch) {
    const profile = resolveRetrievalModelProfile('reranker', profileId);
    if (!['auto', 'cpu'].includes(devicePreference)) throw new TypeError('Unsupported inference device preference.');
    if (profile.requiredDevice && devicePreference === 'cpu')
      throw Object.assign(new Error('The selected GPU reranker cannot use a CPU preference. / GPU 重排配置不能使用 CPU 偏好。'),
        { code: 'RERANK_GPU_REQUIRED' });
    const operation = this.configuration.then(async () => {
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      if (this.closed) throw Object.assign(new Error('Reranker router is closed. / 重排路由已关闭。'), { code: 'RERANK_CLOSED' });
      for (const [id, instance] of this.instances)
        if (id === profileId ? this.preferences.get(id) !== devicePreference : retireOtherProfiles) this._retire(id, instance);
      // Preserve the selected cold-session preference even if a status observer runs immediately after drainage.
      // 即使状态观察紧接排空回执运行，也保留已选择的冷会话偏好。
      this.preferences.set(profileId, devicePreference);
      await Promise.all([...this.retiring].filter(([id]) => id === profileId || retireOtherProfiles).map(([, entry]) => entry.completion));
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      const instance = this._instance(profileId, devicePreference);
      // Dispatch synchronously claims this session before another preference can retire it.
      // 同步派发先认领当前会话，其他偏好随后才能退役它，避免取消等待者造成空会话翻转。
      const request = dispatch?.(instance);
      request?.catch(() => {});
      return { profileId, devicePreference, executionIdentity: `${profileId}:${devicePreference}`,
        ...(request ? { request } : {}) };
    });
    this.configuration = operation.catch(() => {});
    return waitForRetirement(operation, signal);
  }
  retire({ profileId, signal } = {}) {
    if (profileId !== undefined) resolveRetrievalModelProfile('reranker', profileId);
    const operation = this.configuration.then(async () => {
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      for (const [id, instance] of this.instances) if (profileId === undefined || id === profileId) this._retire(id, instance);
      await Promise.all([...this.retiring].filter(([id]) => profileId === undefined || id === profileId).map(([, entry]) => entry.completion));
      return { retired: true, profileId: profileId ?? null };
    });
    this.configuration = operation.catch(() => {});
    return waitForRetirement(operation, signal);
  }
  async rerank(input) {
    const profileId = input.profileId ?? 'builtin-multilingual-reranker';
    const devicePreference = input.devicePreference ?? this.preferences.get(profileId) ?? 'auto';
    const configured = await this.configure({ profileId, devicePreference, signal: input.signal },
      instance => instance.rerank({ ...input, profileId, devicePreference }));
    return configured.request;
  }
  async releaseIdleGpu({ signal } = {}) {
    const results = await Promise.all([...this.instances].map(async ([profileId, instance]) =>
      ({ profileId, ...await instance.releaseIdleGpu({ signal }) })));
    return { released: results.some(result => result.released), results };
  }
  async close() {
    this.closed = true;
    await Promise.all([...this.instances.values()].map(instance => instance.close())
      .concat([...this.retiring.values()].map(retirement => Promise.all([retirement.instance.close(), retirement.completion]))));
    await this.configuration.catch(() => {});
  }
  async releaseIdleResources({ signal } = {}) {
    signal?.throwIfAborted();
    const instances = [...this.instances];
    const settled = await Promise.allSettled(instances.map(async ([, instance]) => instance.releaseIdleResources({ signal })));
    signal?.throwIfAborted();
    const results = settled.map((result, index) => result.status === 'fulfilled'
      ? { profileId: instances[index][0], ...result.value }
      : { profileId: instances[index][0], released: false, code: result.reason?.code ?? 'RERANK_CLOSE_FAILED' });
    return { released: results.some(result => result.released), results };
  }
}
