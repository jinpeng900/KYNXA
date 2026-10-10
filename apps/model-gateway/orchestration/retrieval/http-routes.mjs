import { readJsonBody, sendJson } from '../http-transport.mjs';
import { objectInput, toolFailure } from '../../platform/tool-paths.mjs';

// Settings are independent of chats; no synthetic chat, model request or MCP restart is created.
// 检索设置不依赖聊天，不创建临时聊天、模型请求或重启 MCP 服务。
export async function handleRetrievalRoute(request, response, url, retrieval) {
  const path = url.pathname, method = request.method;
  if (!path.startsWith('/api/retrieval/') && !/^\/api\/projects\/[^/]+\/retrieval\//u.test(path)) return false;
  const controller = new AbortController();
  const aborted = () => controller.abort();
  const disconnected = () => { if (!response.writableEnded) aborted(); };
  request.on('aborted', aborted);
  response.on('close', disconnected);
  if (request.aborted) aborted();
  try { return await dispatchRetrievalRoute(request, response, url, retrieval, controller.signal); }
  finally { request.off('aborted', aborted); response.off('close', disconnected); }
}

async function dispatchRetrievalRoute(request, response, url, retrieval, signal) {
  const path = url.pathname, method = request.method;
  signal.throwIfAborted();
  const send = value => { sendJson(response, 200, value); return true; };
  if (path === '/api/retrieval/settings') {
    if (method === 'GET') return send(await retrieval.settings.getGlobal());
    if (method === 'PATCH') {
      const previous = await retrieval.effective();
      const input = await readJsonBody(request), previousScopes = new Map();
      const mayReenable = previous.local.enabled === false && input?.patch?.local?.enabled === true ||
        previous.local.semantic === 'off' && input?.patch?.local?.semantic === 'auto';
      if (mayReenable) {
        const stoppedScopes = new Set((await retrieval.jobs.list()).filter(job =>
          job.automaticRebuildBlocked === true || job.status === 'cancelled' && job.error === 'INDEX_CANCELLED' &&
          job.automaticRebuildBlocked !== false).map(job => job.projectId));
        for (const projectId of stoppedScopes) {
          signal.throwIfAborted();
          try { previousScopes.set(projectId, projectId === null ? previous : await retrieval.effective(projectId)); }
          catch (error) { if (error.code === 'PROJECT_NOT_FOUND') continue; throw error; }
        }
      }
      const result = await retrieval.settings.patchGlobal(input);
      await retrieval.configureInferenceSettings?.(null, { previous });
      retrieval.scheduleMountedProjects?.();
      // Resume only stopped scopes whose effective state really changed; explicit project overrides still apply.
      // 仅恢复有效状态确实从关闭变为开启的已取消范围，工作独立覆盖仍然生效。
      for (const [projectId, scopedPrevious] of previousScopes) {
        signal.throwIfAborted();
        let scoped;
        try { scoped = await retrieval.effective(projectId); }
        catch (error) { if (error.code === 'PROJECT_NOT_FOUND') continue; throw error; }
        if (scoped.local.enabled !== false && scoped.local.semantic !== 'off' &&
            (scopedPrevious.local.enabled === false || scopedPrevious.local.semantic === 'off'))
          await retrieval.rebuild({ projectId, dirty: true });
      }
      return send(result);
    }
  }
  const projectSettings = /^\/api\/projects\/([a-zA-Z0-9_-]+)\/retrieval\/settings$/u.exec(path);
  if (projectSettings) {
    if (method === 'GET') return send({ ...await retrieval.settings.getProject(projectSettings[1]),
      effective: await retrieval.effective(projectSettings[1]) });
    if (method === 'PATCH') {
      const previous = await retrieval.effective(projectSettings[1]);
      const result = await retrieval.settings.patchProject(projectSettings[1], await readJsonBody(request));
      const effective = await retrieval.effective(projectSettings[1]);
      await retrieval.configureInferenceSettings?.(projectSettings[1], { previous });
      const sourcesChanged = JSON.stringify(previous.projectIndexing) !== JSON.stringify(effective.projectIndexing);
      const reenabled = previous.local.enabled === false && effective.local.enabled !== false ||
        previous.local.semantic === 'off' && effective.local.semantic !== 'off' ||
        previous.projectIndexing?.mountedFolder === false && effective.projectIndexing?.mountedFolder === true;
      if (effective.local.enabled !== false &&
          (reenabled || effective.projectIndexing?.mountedFolder || previous.projectIndexing?.mountedFolder || sourcesChanged))
        await retrieval.rebuild({ projectId: projectSettings[1], dirty: true, automatic: !reenabled });
      return send({ ...result, effective });
    }
  }
  if (path === '/api/retrieval/status' && method === 'GET') return send(await retrieval.status());
  if (path === '/api/retrieval/models' && method === 'GET') return send({ profiles: retrieval.modelProfiles() });
  if (path === '/api/retrieval/providers' && method === 'GET') return send(await retrieval.tools.webSearch.providers());
  if (path === '/api/retrieval/index/rebuild' && method === 'POST')
    return send(await retrieval.rebuild({ ...objectInput(await readJsonBody(request)), signal }));
  const job = /^\/api\/retrieval\/index\/jobs\/([a-zA-Z0-9-]+)(\/cancel)?$/u.exec(path);
  if (job && method === (job[2] ? 'POST' : 'GET'))
    return send(job[2] ? await retrieval.cancelJob(job[1]) : await retrieval.jobs.get(job[1]));
  if (path === '/api/retrieval/sources') {
    if (method === 'GET') return send(await retrieval.library.list(url.searchParams.get('projectId')));
    if (method === 'POST') return send(await retrieval.importSource(objectInput(await readJsonBody(request)), { signal }));
  }
  const source = /^\/api\/retrieval\/sources\/([a-zA-Z0-9-]+)$/u.exec(path);
  if (source && method === 'DELETE') return send(await retrieval.removeSource(source[1], await readJsonBody(request), { signal }));
  throw toolFailure('检索接口或操作不存在。', 'RETRIEVAL_ROUTE_NOT_FOUND', 404);
}
