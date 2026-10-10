import { EmbeddingService } from './embedding-service.mjs';
import { resolveRetrievalModelProfile } from './model-registry.mjs';

const DEFAULT_IDLE_RELEASE_MS = 120_000;

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
  constructor({ resourceService, idleReleaseMs = DEFAULT_IDLE_RELEASE_MS, factory = options => new EmbeddingService(options) } = {}) {
    this.resources = resourceService; this.factory = factory; this.instances = new Map(); this.retiring = new Map(); this.closed = false;
    this.preferences = new Map(); this.configuration = Promise.resolve();
    this.idleReleaseMs = Number.isFinite(idleReleaseMs) && idleReleaseMs > 0 ? idleReleaseMs : DEFAULT_IDLE_RELEASE_MS;
    this.idleReleaseTimer = undefined;
    this.idleReleaseDiagnostic = undefined;
  }
  _instance(profileId = 'builtin-multilingual', devicePreference = this.preferences.get(profileId) ?? 'auto') {
    resolveRetrievalModelProfile('embedding', profileId);
    if (this.closed) throw Object.assign(new Error('Embedding router is closed. / 嵌入路由已关闭。'), { code: 'EMBEDDING_CLOSED' });
    if (!this.instances.has(profileId)) {
      this.instances.set(profileId, this.factory({ resourceService: this.resources, profileId, devicePreference }));
      this.preferences.set(profileId, devicePreference);
      this._startIdleRelease();
    }
    return this.instances.get(profileId);
  }
  status(profileId = 'builtin-multilingual', { devicePreference } = {}) {
    try {
      const retirement = this.retiring.get(profileId);
      const preference = retirement?.preference ?? this.preferences.get(profileId) ?? devicePreference ?? 'auto';
      const result = (retirement?.instance ?? this._instance(profileId, preference)).status(profileId);
      // Observation never switches a configured CPU session back to the legacy automatic default.
      // 状态查询不能把已配置的 CPU 会话切回旧的自动默认值，也不能在退役期间创建替代进程。
      return { ...result, devicePreference: preference, executionIdentity: `${profileId}:${preference}`,
        idleReleaseMs: this.idleReleaseMs,
        ...(this.idleReleaseDiagnostic ? { idleReleaseDiagnostic: this.idleReleaseDiagnostic } : {}),
        ...(devicePreference && devicePreference !== preference ? { requestedDevicePreference: devicePreference,
          requiresReconfiguration: true } : {}) };
    }
    catch (error) {
      if (error.code !== 'RETRIEVAL_MODEL_PROFILE_UNSUPPORTED') throw error;
      return this._instance().status(profileId);
    }
  }
  _retire(profileId, instance) {
    this.instances.delete(profileId);
    const retirement = { instance, preference: this.preferences.get(profileId), completion: undefined };
    retirement.completion = Promise.resolve().then(() => instance.retire?.() ?? instance.close()).then(() => {
      if (this.retiring.get(profileId) === retirement) this.retiring.delete(profileId);
    });
    // Keep a failed drain as a barrier; the next configuration cannot hide uncertain native cleanup.
    // 排空失败保留屏障，下一次配置不能掩盖原生清理状态不确定的问题。
    this.retiring.set(profileId, retirement);
    return retirement.completion;
  }
  configure({ profileId = 'builtin-multilingual', devicePreference = 'auto', retireOtherProfiles = false, signal } = {}, dispatch) {
    const profile = resolveRetrievalModelProfile('embedding', profileId);
    if (!['auto', 'cpu'].includes(devicePreference)) throw new TypeError('Unsupported inference device preference.');
    if (profile.requiredDevice && devicePreference === 'cpu')
      throw Object.assign(new Error('The selected GPU space cannot use a CPU preference. / GPU 空间不能使用 CPU 偏好。'),
        { code: 'EMBEDDING_GPU_REQUIRED' });
    const operation = this.configuration.then(async () => {
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      if (this.closed) throw Object.assign(new Error('Embedding router is closed. / 嵌入路由已关闭。'), { code: 'EMBEDDING_CLOSED' });
      for (const [id, instance] of this.instances)
        if (id === profileId ? this.preferences.get(id) !== devicePreference : retireOtherProfiles) this._retire(id, instance);
      // A cold status read between drain completion and activation must construct the selected preference.
      // 排空完成到激活之间的冷状态读取必须沿已选偏好创建会话，不能恢复旧偏好。
      this.preferences.set(profileId, devicePreference);
      await Promise.all([...this.retiring].filter(([id]) => id === profileId || retireOtherProfiles).map(([, entry]) => entry.completion));
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      const instance = this._instance(profileId, devicePreference);
      // Admission belongs to this configuration step; two opposite waiters must not alternate empty sessions forever.
      // 请求接纳归当前配置步骤所有；两个相反偏好的等待者不能在空会话之间无限翻转。
      const request = dispatch?.(instance);
      request?.catch(() => {});
      return { profileId, devicePreference, executionIdentity: `${profileId}:${devicePreference}`,
        ...(request ? { request } : {}) };
    });
    this.configuration = operation.catch(() => {});
    return waitForRetirement(operation, signal);
  }
  retire({ profileId, signal } = {}) {
    if (profileId !== undefined) resolveRetrievalModelProfile('embedding', profileId);
    const operation = this.configuration.then(async () => {
      if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
      for (const [id, instance] of this.instances) if (profileId === undefined || id === profileId) this._retire(id, instance);
      await Promise.all([...this.retiring].filter(([id]) => profileId === undefined || id === profileId).map(([, entry]) => entry.completion));
      return { retired: true, profileId: profileId ?? null };
    });
    this.configuration = operation.catch(() => {});
    return waitForRetirement(operation, signal);
  }
  async _request(method, input, options) {
    const profileId = options.profileId ?? 'builtin-multilingual';
    const devicePreference = options.devicePreference ?? this.preferences.get(profileId) ?? 'auto';
    const configured = await this.configure({ profileId, devicePreference, signal: options.signal },
      instance => instance[method](input, { ...options, devicePreference }));
    return configured.request;
  }
  fitDocuments(documents, options = {}) { return this._request('fitDocuments', documents, options); }
  embedDocuments(texts, options = {}) { return this._request('embedDocuments', texts, options); }
  embedQuery(query, options = {}) { return this._request('embedQuery', query, options); }

  // New calls wait for confirmed retirement, then create a lazy session with the same vector identity.
  // 新调用等待安全退役回执，再按同一向量身份创建按需会话；旧请求不会自动重放。
  async releaseIdleGpu({ signal } = {}) {
    return this._releaseIdle({ signal, gpuOnly: true, reason: 'gpu-pressure' });
  }
  async releaseIdleResources({ signal, minimumIdleMs = 0, reason = 'memory-pressure' } = {}) {
    return this._releaseIdle({ signal, minimumIdleMs, reason });
  }
  _startIdleRelease() {
    if (this.idleReleaseTimer || this.closed) return;
    // Keep short query bursts warm; only acknowledged idle workers are eligible for periodic retirement.
    // 连续查询保温；定期回收只处理已经收到原生空闲回执的 worker，取消中的工作也不能提前释放。
    this.idleReleaseTimer = setInterval(() => {
      this.releaseIdleResources({ minimumIdleMs: this.idleReleaseMs, reason: 'idle-timeout' })
        .catch(error => { this.idleReleaseDiagnostic = { code: error.code ?? 'EMBEDDING_CLOSE_FAILED' }; });
    }, Math.max(25, Math.min(30_000, this.idleReleaseMs)));
    this.idleReleaseTimer.unref();
  }
  async _releaseIdle({ signal, gpuOnly = false, minimumIdleMs = 0, reason }) {
    if (signal?.aborted) await waitForRetirement(Promise.resolve(), signal);
    const results = [], completions = [...this.retiring.values()].map(retirement => retirement.completion);
    for (const [profileId, instance] of this.instances) {
      const claim = (gpuOnly ? instance.tryRetireIdleGpu?.({ signal })
        : instance.tryRetireIdleResources?.({ signal, minimumIdleMs })) ?? { retiring: false, reason: 'retirement-unsupported' };
      if (!claim.retiring) { results.push({ profileId, released: false, reason: claim.reason }); continue; }
      this.instances.delete(profileId);
      const retirement = { instance, preference: this.preferences.get(profileId), completion: undefined };
      retirement.completion = claim.completion.then(() => {
        if (this.retiring.get(profileId) === retirement) this.retiring.delete(profileId);
        this.idleReleaseDiagnostic = undefined;
        return { profileId, released: true, gpuMemoryBytes: claim.gpuMemoryBytes,
          residentMemoryBytes: claim.residentMemoryBytes, reason };
      });
      // Failed retirement remains a barrier; do not load a replacement while owned cleanup is uncertain.
      // 退役失败保留屏障，不能在本应用旧进程清理状态不确定时加载替代模型。
      this.retiring.set(profileId, retirement);
      completions.push(retirement.completion);
    }
    results.push(...(await Promise.all(completions)).filter(Boolean));
    return { released: results.some(result => result.released), results };
  }
  async close() {
    this.closed = true;
    clearInterval(this.idleReleaseTimer);
    this.idleReleaseTimer = undefined;
    await Promise.all([...this.instances.values()].map(instance => instance.close())
      .concat([...this.retiring.values()].map(retirement => Promise.all([retirement.instance.close(), retirement.completion]))));
    await this.configuration.catch(() => {});
  }
}
