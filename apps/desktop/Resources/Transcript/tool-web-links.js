/* Website activity presentation only; this does not classify or authorize tool execution. */
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
    if (typeof tool.name !== 'string' || !tool.name.startsWith('mcp.')) return null;
    const leaf = tool.name.slice(tool.name.lastIndexOf('.') + 1);
    if (!websiteTools.has(leaf)) return null;
    return searchTools.has(leaf) ? 'search' : leaf === 'fetch' ? 'fetch' : 'browser';
  }

  function businessArguments(tool) {
    const args = tool.arguments;
    if (!args || typeof args !== 'object' || Array.isArray(args)) return {};
    return args.policy && args.arguments && typeof args.arguments === 'object' && !Array.isArray(args.arguments)
      ? args.arguments : args;
  }

  // Traverse only public result containers, never arbitrary metadata, scripts, or policy fields.
  function resultParts(result) {
    const texts = [], urls = [], pending = [{ value: result, depth: 0 }];
    let visited = 0;
    while (pending.length && visited++ < 512) {
      const { value, depth } = pending.shift();
      if (depth > 8 || value == null) continue;
      if (typeof value === 'string') {
        const text = value.slice(0, 65536);
        try { pending.push({ value: JSON.parse(text), depth: depth + 1 }); }
        catch { texts.push(text); }
      } else if (Array.isArray(value)) {
        for (const child of value.slice(0, 64)) pending.push({ value: child, depth: depth + 1 });
      } else if (typeof value === 'object') {
        for (const field of ['url', 'uri', 'link']) if (typeof value[field] === 'string') urls.push(value[field]);
        for (const field of ['text', 'content', 'structuredContent', 'results', 'items', 'web', 'data', 'output', 'preview', 'resource'])
          if (value[field] != null) pending.push({ value: value[field], depth: depth + 1 });
      }
    }
    return { texts, urls };
  }

  function selectedBrowserUrl(texts, args) {
    for (const text of texts) {
      const pageUrl = /^(?:-\s*)?Page URL:\s*(https?:\/\/\S+)\s*$/imu.exec(text);
      if (pageUrl) return pageUrl[1];
      const snapshotUrl = /^\s*uid=\S+\s+RootWebArea[^\r\n]*\burl="(https?:\/\/[^"\r\n]+)"/mu.exec(text);
      if (snapshotUrl) return snapshotUrl[1];
    }
    for (const text of texts) {
      const lines = text.split(/\r?\n/u);
      const selected = lines.find(line => /^\s*\d+:/u.test(line) && /\[selected\]/u.test(line));
      const requested = Number.isSafeInteger(args.pageId) ? lines.find(line => new RegExp(`^\\s*${args.pageId}:`).test(line)) : null;
      const currentTab = lines.find(line => /^\s*-\s*\d+:\s*\(current\)/u.test(line));
      for (const line of [requested, selected, currentTab].filter(Boolean)) {
        const wrapped = /\((https?:\/\/\S+)\)\s*(?:\[selected\])?\s*$/iu.exec(line)
          ?? /\]\((https?:\/\/\S+)\)\s*$/iu.exec(line);
        if (wrapped) return wrapped[1];
        const match = /https?:\/\/[^\s\]]+/iu.exec(line);
        if (match) return match[0];
      }
      const navigated = /^Successfully navigated to\s+(https?:\/\/\S+)/imu.exec(text);
      if (navigated) return navigated[1];
      // evaluate_script JSON can explicitly return its own current location.
      const json = /```json\s*([\s\S]*?)```/iu.exec(text);
      if (json) try {
        const page = JSON.parse(json[1]);
        const url = page?.url ?? page?.location?.href;
        if (typeof url === 'string') return url;
      } catch { }
    }
    return null;
  }

  function extract(tool) {
    const kind = toolKind(tool);
    if (!kind) return null;
    const args = businessArguments(tool), links = [], seen = new Set();
    const add = candidate => {
      const url = websiteUrl(candidate);
      if (url && !seen.has(url) && links.length < maximumLinks) { seen.add(url); links.push(url); }
    };
    const parts = resultParts(tool.result);
    if (kind === 'browser') {
      const current = selectedBrowserUrl(parts.texts, args);
      if (current) add(current);
      else {
        add(args.url);
        for (const url of parts.urls.slice(0, 1)) add(url);
      }
    } else if (kind === 'fetch') {
      add(args.url);
      for (const text of parts.texts) {
        const match = /^Contents of (https?:\/\/[^\r\n]+):\s*$/imu.exec(text);
        if (match) add(match[1]);
      }
    } else {
      for (const url of parts.urls) add(url);
      for (const text of parts.texts) {
        for (const match of text.matchAll(/^(?:URL|Source(?: URL)?|Link):\s*(https?:\/\/\S+)\s*$/gimu)) add(match[1]);
        for (const match of text.matchAll(/^\s*(?:\d+[.)]\s*)?\[[^\]\r\n]+\]\((https?:\/\/[^\s)]+)\)/gmu)) add(match[1]);
      }
      for (const url of Array.isArray(args.urls) ? args.urls.slice(0, maximumLinks) : []) add(url);
    }
    return links;
  }

  function query(tool) {
    if (toolKind(tool) !== 'search') return '';
    const args = businessArguments(tool);
    for (const field of ['query', 'search_query'])
      if (typeof args[field] === 'string') return args[field].slice(0, 2000);
    return '';
  }

  window.KynxaToolWebLinks = Object.freeze({ extract, query });
})();
