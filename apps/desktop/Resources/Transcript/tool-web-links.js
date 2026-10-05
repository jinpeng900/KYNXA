/* Website activity presentation only; this does not classify or authorize tool execution.
 * 只展示网页活动；此处不判断工具执行类别，也不授予执行权限。 */
(() => {
  'use strict';
  const websiteTools = new Set(['fetch', 'web_search_exa', 'web_search_exa_deep', 'brave_web_search',
    'web_search', 'search_web', 'browser_navigate', 'browser_navigate_back', 'browser_snapshot',
    'browser_evaluate', 'browser_tabs', 'browser_click', 'navigate_page', 'new_page', 'list_pages',
    'take_snapshot', 'evaluate_script', 'navigate', 'read_page']);
  const searchTools = new Set(['web_search_exa', 'web_search_exa_deep', 'brave_web_search', 'web_search', 'search_web']);
  const maximumLinks = 32;

  function websiteUrl(value) {
    if (typeof value !== 'string' || value.length > 8192 || /[\u0000-\u0020]/u.test(value)) return null;
    try {
      const url = new URL(value);
      if (!['http:', 'https:'].includes(url.protocol) || !url.hostname || url.username || url.password) return null;
      for (const key of url.searchParams.keys())
        if (/^(?:api[-_]?key|access[-_]?token|authorization|password|passwd|secret)$/iu.test(key)) return null;
      return url.href;
    } catch { return null; }
  }

  function toolKind(tool) {
    if (tool.name === 'web.fetch') return 'fetch';
    if (tool.name === 'web.search') return 'search';
    if (typeof tool.name !== 'string' || !tool.name.startsWith('mcp.')) return null;
    const toolName = tool.name.slice(tool.name.lastIndexOf('.') + 1);
    if (!websiteTools.has(toolName)) return null;
    return searchTools.has(toolName) ? 'search' : toolName === 'fetch' ? 'fetch' : 'browser';
  }

  function businessArguments(tool) {
    const businessParameters = tool.arguments;
    if (!businessParameters || typeof businessParameters !== 'object' || Array.isArray(businessParameters)) return {};
    return businessParameters.policy && businessParameters.arguments && typeof businessParameters.arguments === 'object' && !Array.isArray(businessParameters.arguments)
      ? businessParameters.arguments : businessParameters;
  }

  // Traverse only public result containers, never arbitrary metadata, scripts, or policy fields.
  // 只遍历公开结果容器，不检查任意元数据、脚本或权限策略字段。
  function resultParts(result) {
    const texts = [], urls = [], resultQueue = [{ value: result, depth: 0 }];
    let visitedCount = 0;
    while (resultQueue.length && visitedCount++ < 512) {
      const { value, depth } = resultQueue.shift();
      if (depth > 8 || value == null) continue;
      if (typeof value === 'string') {
        const text = value.slice(0, 65536);
        try { resultQueue.push({ value: JSON.parse(text), depth: depth + 1 }); }
        catch { texts.push(text); }
      } else if (Array.isArray(value)) {
        for (const child of value.slice(0, 64)) resultQueue.push({ value: child, depth: depth + 1 });
      } else if (typeof value === 'object') {
        for (const field of ['url', 'uri', 'link']) if (typeof value[field] === 'string') urls.push(value[field]);
        for (const field of ['text', 'content', 'structuredContent', 'results', 'sources', 'items', 'web', 'data', 'output', 'preview', 'resource'])
          if (value[field] != null) resultQueue.push({ value: value[field], depth: depth + 1 });
      }
    }
    return { texts, urls };
  }

  function selectedBrowserUrl(texts, businessParameters) {
    for (const text of texts) {
      const pageUrlMatch = /^(?:-\s*)?Page URL:\s*(https?:\/\/\S+)\s*$/imu.exec(text);
      if (pageUrlMatch) return pageUrlMatch[1];
      const snapshotUrlMatch = /^\s*uid=\S+\s+RootWebArea[^\r\n]*\burl="(https?:\/\/[^"\r\n]+)"/mu.exec(text);
      if (snapshotUrlMatch) return snapshotUrlMatch[1];
    }
    for (const text of texts) {
      const lines = text.split(/\r?\n/u);
      const selectedPageLine = lines.find(line => /^\s*\d+:/u.test(line) && /\[selected\]/u.test(line));
      const requestedPageLine = Number.isSafeInteger(businessParameters.pageId) ? lines.find(line => new RegExp(`^\\s*${businessParameters.pageId}:`).test(line)) : null;
      const currentTabLine = lines.find(line => /^\s*-\s*\d+:\s*\(current\)/u.test(line));
      for (const line of [requestedPageLine, selectedPageLine, currentTabLine].filter(Boolean)) {
        const wrappedUrlMatch = /\((https?:\/\/\S+)\)\s*(?:\[selected\])?\s*$/iu.exec(line)
          ?? /\]\((https?:\/\/\S+)\)\s*$/iu.exec(line);
        if (wrappedUrlMatch) return wrappedUrlMatch[1];
        const urlMatch = /https?:\/\/[^\s\]]+/iu.exec(line);
        if (urlMatch) return urlMatch[0];
      }
      const navigationMatch = /^Successfully navigated to\s+(https?:\/\/\S+)/imu.exec(text);
      if (navigationMatch) return navigationMatch[1];
      // evaluate_script JSON can explicitly return its own current location.
      // evaluate_script 的 JSON 结果可以明确返回当前页面位置。
      const jsonBlockMatch = /```json\s*([\s\S]*?)```/iu.exec(text);
      if (jsonBlockMatch) try {
        const pageResult = JSON.parse(jsonBlockMatch[1]);
        const url = pageResult?.url ?? pageResult?.location?.href;
        if (typeof url === 'string') return url;
      } catch { }
    }
    return null;
  }

  function extract(tool) {
    const kind = toolKind(tool);
    if (!kind) return null;
    const businessParameters = businessArguments(tool), websiteLinks = [], seenUrls = new Set();
    const addWebsiteLink = candidate => {
      const url = websiteUrl(candidate);
      if (url && !seenUrls.has(url) && websiteLinks.length < maximumLinks) { seenUrls.add(url); websiteLinks.push(url); }
    };
    const resultContent = resultParts(tool.result);
    if (kind === 'browser') {
      const currentPageUrl = selectedBrowserUrl(resultContent.texts, businessParameters);
      if (currentPageUrl) addWebsiteLink(currentPageUrl);
      else {
        addWebsiteLink(businessParameters.url);
        for (const url of resultContent.urls.slice(0, 1)) addWebsiteLink(url);
      }
    } else if (kind === 'fetch') {
      addWebsiteLink(businessParameters.url);
      for (const text of resultContent.texts) {
        const urlMatch = /^Contents of (https?:\/\/[^\r\n]+):\s*$/imu.exec(text);
        if (urlMatch) addWebsiteLink(urlMatch[1]);
      }
    } else {
      for (const url of resultContent.urls) addWebsiteLink(url);
      for (const text of resultContent.texts) {
        for (const urlMatch of text.matchAll(/^(?:URL|Source(?: URL)?|Link):\s*(https?:\/\/\S+)\s*$/gimu)) addWebsiteLink(urlMatch[1]);
        for (const urlMatch of text.matchAll(/^\s*(?:\d+[.)]\s*)?\[[^\]\r\n]+\]\((https?:\/\/[^\s)]+)\)/gmu)) addWebsiteLink(urlMatch[1]);
      }
      for (const url of Array.isArray(businessParameters.urls) ? businessParameters.urls.slice(0, maximumLinks) : []) addWebsiteLink(url);
    }
    return websiteLinks;
  }

  function query(tool) {
    if (toolKind(tool) !== 'search') return '';
    const businessParameters = businessArguments(tool);
    for (const field of ['query', 'search_query'])
      if (typeof businessParameters[field] === 'string') return businessParameters[field].slice(0, 2000);
    return '';
  }

  window.KynxaToolWebLinks = Object.freeze({ extract, query });
})();
