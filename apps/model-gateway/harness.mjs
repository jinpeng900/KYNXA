import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export class HarnessRuntime {
  constructor({ harnessRoot, dataHome, workspaceRoot, patchPath }) {
    this.options = { harnessRoot, dataHome, workspaceRoot, patchPath };
    this.instances = new Map();
    this.sdkModule = null;
  }

  async sdk() {
    this.sdkModule ??= import(pathToFileURL(resolve(this.options.harnessRoot, 'packages/sdk/client/lib/index.js')).href);
    return this.sdkModule;
  }

  async reply({ conversationId, message, provider, model }) {
    const key = JSON.stringify([provider, model]);
    let harness = this.instances.get(key);
    if (!harness) {
      const { DeepSeekHarness } = await this.sdk();
      harness = new DeepSeekHarness({
        profile: 'sdk',
        patches: [this.options.patchPath],
        dshHome: this.options.dataHome,
        processCwd: this.options.workspaceRoot,
        cwd: this.options.workspaceRoot,
        provider,
        model,
        initializeTimeoutMs: 30000
      });
      this.instances.set(key, harness);
    }
    try {
      const result = await harness.run(message, { sessionId: `kynxa-${conversationId.replaceAll('-', '')}` });
      if (!result.finalResponse.trim()) throw new Error('模型没有返回文本内容。');
      return result.finalResponse;
    } catch (error) {
      if (this.instances.get(key) === harness) {
        this.instances.delete(key);
        await harness.close().catch(() => {});
      }
      throw error;
    }
  }

  async close() {
    await Promise.all([...this.instances.values()].map(harness => harness.close()));
    this.instances.clear();
  }

  async invalidate(providerId) {
    const stale = [];
    for (const [key, harness] of this.instances) {
      if (JSON.parse(key)[0] !== providerId) continue;
      this.instances.delete(key);
      stale.push(harness.close());
    }
    await Promise.all(stale);
  }
}
