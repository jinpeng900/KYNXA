/* Shared Office preview lifecycle; format modules only interpret already authorized bytes.
 * Office 预览共用加载与生命周期；格式模块仅解释已授权原件，不自行读取本机或外部地址。 */
(() => {
  'use strict';
  const renderers = new Map();
  const formats = new Set(['docx', 'pptx', 'xlsx']);
  const MAX_SOURCE_BYTES = 256 * 1024 * 1024;

  function register(kind, renderer) {
    if (!formats.has(kind) || typeof renderer !== 'function') throw new Error('Invalid Office preview renderer');
    renderers.set(kind, renderer);
  }

  async function open(options) {
    const { kind, contentUrl, signal } = options;
    if (!formats.has(kind)) throw new Error('Unsupported Office preview format');
    const url = new URL(contentUrl, location.href);
    if (url.origin !== 'https://kynxa-workspace-content.local' || url.pathname !== '/Workspace/document.' + kind)
      throw new Error('Office preview requires its authorized document proxy');
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const response = await fetch(url, { mode: 'cors', credentials: 'omit', redirect: 'error', signal });
    if (!response.ok) throw new Error('Office document content unavailable');
    const length = Number(response.headers.get('content-length'));
    if (Number.isFinite(length) && length > MAX_SOURCE_BYTES) throw new Error('Office source exceeds preview capacity');
    const bytes = await response.arrayBuffer();
    if (bytes.byteLength > MAX_SOURCE_BYTES) throw new Error('Office source exceeds preview capacity');
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const renderer = renderers.get(kind);
    if (!renderer) throw new Error('Office preview renderer unavailable');
    // A cached native view owns the resulting DOM and state; switching tabs does not call open again.
    // 缓存视图持有生成的 DOM 与状态；普通标签切换不会再次进入 open 或重复下载解包。
    return renderer(bytes, options);
  }

  register('pptx', (bytes, options) => {
    if (!window.kynxaPptx) throw new Error('PPTX renderer unavailable');
    return window.kynxaPptx.render(bytes, options);
  });
  register('xlsx', (bytes, options) => {
    if (!window.kynxaXlsx) throw new Error('XLSX renderer unavailable');
    return window.kynxaXlsx.render(bytes, options);
  });
  window.kynxaOffice = Object.freeze({ register, open });
})();
