const CPU_PROFILE = 'builtin-multilingual';
const GPU_PROFILE = 'builtin-multilingual-dml-q8';
const supported = new Set([CPU_PROFILE, GPU_PROFILE]);

/** Device preference selects an exact profile; committed source chunks retain two compatible spaces during migration.
 * 设备偏好选择精确配置，迁移期间当前分块保留两个兼容空间，新空间不完整时继续使用旧空间。 */
export class EmbeddingSpacePolicy {
  constructor({ resources, embeddings, index, platform = process.platform }) {
    this.resources = resources; this.embeddings = embeddings; this.index = index; this.platform = platform;
    this.migrations = new Map(); this.hardwareAt = 0;
  }
  async _gpu(signal) {
    if (this.platform !== 'win32') return false;
    if (performance.now() - this.hardwareAt > 2000 || !this.hardware) {
      this.hardware = await this.resources.snapshot({ signal }); this.hardwareAt = performance.now();
    }
    const gpu = this.hardware.gpu ?? this.hardware.hardware?.gpu;
    return gpu?.state === 'available' && gpu.executionProvider === 'dml' && gpu.mappingStatus === 'verified';
  }
  async target(settings, signal) {
    const local = settings.local, configured = local.embeddingProfileId;
    if (!supported.has(configured) || local.semantic === 'off') return configured;
    const policy = local.embeddingDevicePolicy ?? 'auto';
    if (policy === 'cpu') return CPU_PROFILE;
    if (policy === 'gpu' || configured === GPU_PROFILE) return GPU_PROFILE;
    const status = this.embeddings.status(GPU_PROFILE, { devicePreference: 'auto' });
    // Hardware presence cannot turn a legacy CPU adapter into a declared GPU vector space.
    // 有 GPU 不代表旧 CPU 适配器支持 GPU 空间；自动选择必须先核对后端声明的配置与空间身份。
    if (status.profileId !== GPU_PROFILE || status.embeddingSpaceId !== resolveRetrievalModelProfile('embedding', GPU_PROFILE).embeddingSpaceId ||
      !['ready', 'loading'].includes(status.state)) return CPU_PROFILE;
    return await this._gpu(signal) ? GPU_PROFILE : CPU_PROFILE;
  }
  async indexingSettings(settings, signal) {
    const profileId = await this.target(settings, signal);
    return { ...settings, local: { ...settings.local, embeddingProfileId: profileId } };
  }
  async select(settings, scopes, signal) {
    // GPU profiles keep their strict backend contract; CPU-only preference also constrains the actual execution session.
    // GPU 配置保持严格后端合同；CPU 偏好同时约束实际执行会话，不能只修改向量配置名称。
    const devicePreference = (settings.local.embeddingDevicePolicy ?? 'auto') === 'cpu' ? 'cpu' : 'auto';
    if (settings.local.semantic === 'off' || settings.local.enabled === false)
      return { profileId: settings.local.embeddingProfileId, targetProfileId: settings.local.embeddingProfileId,
        devicePreference, state: 'disabled', fallbackProfileId: null, canMigrate: false };
    const targetProfileId = await this.target(settings, signal);
    if (!supported.has(targetProfileId) || !this.index.vectorSpaceStatus)
      return { profileId: targetProfileId, targetProfileId, devicePreference, state: 'configured', fallbackProfileId: null };
    const status = await this.index.vectorSpaceStatus({ scopeKeys: scopes, signal });
    const corpora = status.scopes.filter(scope => scope.chunks > 0);
    const matches = (space, id) => space.profileId === id &&
      space.spaceId === resolveRetrievalModelProfile('embedding', id).embeddingSpaceId;
    const complete = id => corpora.length > 0 && corpora.every(scope => scope.spaces.some(space => matches(space, id) && space.complete));
    const present = id => corpora.some(scope => scope.spaces.some(space => matches(space, id) && space.vectors > 0));
    const cpuFallback = (settings.local.embeddingDevicePolicy ?? 'auto') === 'auto' &&
      settings.local.embeddingProfileId !== GPU_PROFILE && present(CPU_PROFILE);
    const profileId = targetProfileId === GPU_PROFILE && !complete(GPU_PROFILE) && cpuFallback ? CPU_PROFILE : targetProfileId;
    const state = profileId !== targetProfileId ? 'migrating-with-compatible-cpu' :
      complete(targetProfileId) ? 'ready' : present(targetProfileId) ? 'partial' : 'awaiting-index';
    const targetStatus = this.embeddings.status(targetProfileId, { devicePreference });
    return { profileId, targetProfileId, devicePreference, state, fallbackProfileId: cpuFallback && profileId === GPU_PROFILE ? CPU_PROFILE : null,
      coverage: status, canMigrate: corpora.length > 0 && !complete(targetProfileId) &&
        !['unavailable', 'closed'].includes(targetStatus.state),
      correctnessCertified: false };
  }
  schedule(projectId, profileId, operation) {
    const key = `${projectId ?? 'user'}:${profileId}`;
    const previous = this.migrations.get(key);
    if (previous?.promise || previous && performance.now() - previous.finishedAt < 30000) return;
    const entry = { startedAt: performance.now(), profileId, projectId, state: 'preparing' };
    this.migrations.set(key, entry);
    while (this.migrations.size > 32) {
      const oldest = [...this.migrations].find(([, record]) => !record.promise);
      if (!oldest) break; this.migrations.delete(oldest[0]);
    }
    entry.promise = Promise.resolve().then(operation).then(receipt => {
      entry.jobId = receipt.jobId; entry.state = 'background-job';
    }).catch(error => { entry.state = 'failed'; entry.errorCode = error.code ?? 'EMBEDDING_MIGRATION_FAILED'; })
      .finally(() => { entry.finishedAt = performance.now(); entry.promise = null; });
  }
  status(jobs = []) { return { platform: 'windows', supportedProfiles: [...supported], retainedSpaces: 2,
    migration: [...this.migrations.values()].map(({ promise, ...entry }) => ({ ...entry,
      ...(entry.jobId ? { jobState: jobs.find(job => job.jobId === entry.jobId)?.status ?? 'not-observed',
        indexedCoverageNeedsVerification: true } : {}) })), automaticWeightRelabeling: false }; }
  async close() { await Promise.all([...this.migrations.values()].map(entry => entry.promise)); }
}
import { resolveRetrievalModelProfile } from '../../models/retrieval/model-registry.mjs';
