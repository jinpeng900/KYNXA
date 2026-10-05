import { validatePublicWebUrl } from '../web-http-transport.mjs';

const MAX_SOURCE_TITLE_CHARACTERS = 512;
const MAX_SOURCE_EXCERPT_CHARACTERS = 2500;
const MAX_RESULT_DEPTH = 12;
const SOURCE_FIELDS = new Set(['url', 'title', 'name', 'text', 'description', 'snippet', 'content', 'highlights', 'summary']);

function sourceText(value) {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(sourceText).filter(Boolean).join('\n');
  if (value && typeof value === 'object' && typeof value.text === 'string') return value.text;
  return '';
}

function sourceExcerpt(source) {
  for (const field of ['text', 'description', 'snippet', 'content', 'highlights', 'summary']) {
    const text = sourceText(source[field]);
    if (text.trim()) return text;
  }
  return '';
}

/** Parse provider-labelled blocks before examining bare links; excerpts belong to one source only.
 * 先解析供应商的来源标签块，再处理普通链接；摘录只属于对应来源，不跨来源截取窗口。
 */
function labelledSources(text) {
  const sources = [];
  let current = null;
  let hasSourceLabels = false;
  const finish = () => {
    if (current?.url) sources.push({ url: current.url, title: current.title, excerpt: current.lines.join('\n').trim() });
    current = null;
  };
  for (const line of text.split(/\r?\n/u)) {
    const title = /^\s*Title:\s*(.*)$/iu.exec(line);
    const url = /^\s*URL:\s*(.*)$/iu.exec(line);
    if (title) {
      hasSourceLabels = true;
      finish();
      current = { title: title[1].trim(), url: '', lines: [], hasBody: false };
    } else if (url) {
      hasSourceLabels = true;
      if (current?.url) finish();
      current ??= { title: '', url: '', lines: [], hasBody: false };
      current.url = url[1].trim();
    } else if (/^\s*(?:---+|\*\*\*+)\s*$/u.test(line)) {
      finish();
    } else if (current?.url) {
      const body = /^\s*(?:Content|Text|Highlights|Summary|Description):\s*(.*)$/iu.exec(line);
      if (body) {
        current.hasBody = true;
        if (body[1]) current.lines.push(body[1]);
      } else if (!current.hasBody && /^\s*(?:Published(?: Date)?|Author|Date):/iu.test(line)) {
        continue;
      } else {
        if (line.trim()) current.hasBody = true;
        current.lines.push(line);
      }
    }
  }
  finish();
  return { sources, hasSourceLabels };
}

/** Normalize structured MCP results and Exa text without inventing titles or mixing evidence.
 * 归一化结构化 MCP 结果与 Exa 文本，不编造标题，也不把不同来源的证据混合。
 */
export function normalizeWebSources(content, maximum = 6) {
  const maximumSources = Number.isFinite(maximum) ? Math.max(0, Math.trunc(maximum)) : 6;
  if (!maximumSources) return [];
  const sources = new Map();
  const visited = new WeakSet();
  const add = (value, title = '', excerpt = '') => {
    try {
      const url = validatePublicWebUrl(value).href;
      const source = { url, title: String(title).trim().slice(0, MAX_SOURCE_TITLE_CHARACTERS),
        excerpt: String(excerpt).trim().slice(0, MAX_SOURCE_EXCERPT_CHARACTERS) };
      const previous = sources.get(url);
      if (previous) {
        if (!previous.title && source.title) previous.title = source.title;
        if (!previous.excerpt && source.excerpt) previous.excerpt = source.excerpt;
      } else if (sources.size < maximumSources) sources.set(url, source);
    } catch {
      // Public source links retain the same URL checks as the execution broker.
      // 公开来源链接继续使用执行代理相同的网址校验，排除私网与带凭据的网址。
    }
  };
  const visitText = text => {
    const labelled = labelledSources(text);
    if (labelled.hasSourceLabels) {
      for (const source of labelled.sources) add(source.url, source.title, source.excerpt);
      return;
    }
    // Unlabelled text provides links only; surrounding prose has no reliable source ownership.
    // 无标签文本只提供链接，周围正文无法可靠归属某个来源，因此不作为来源摘录。
    for (const line of text.split(/\r?\n/u)) {
      for (const match of line.matchAll(/\[([^\]\r\n]*)\]\((https?:\/\/[^\s<>]+)\)/gu)) add(match[2], match[1]);
      for (const match of line.matchAll(/https?:\/\/[^\s<>"\]\)]+/gu)) add(match[0].replace(/[.,;]+$/u, ''));
    }
  };
  const visit = (value, depth = 0) => {
    if (depth > MAX_RESULT_DEPTH || value == null) return;
    if (typeof value === 'string') {
      try { visit(JSON.parse(value), depth + 1); } catch { visitText(value); }
      return;
    }
    if (typeof value !== 'object' || visited.has(value)) return;
    visited.add(value);
    if (Array.isArray(value)) {
      for (const item of value) visit(item, depth + 1);
      return;
    }
    const isSource = typeof value.url === 'string';
    if (isSource) add(value.url, value.title ?? value.name ?? '', sourceExcerpt(value));
    for (const [field, child] of Object.entries(value)) {
      if (field === '_meta' || field === 'data' || field === 'blob' || isSource && SOURCE_FIELDS.has(field)) continue;
      visit(child, depth + 1);
    }
  };
  visit(content);
  return [...sources.values()];
}
