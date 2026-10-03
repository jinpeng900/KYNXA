/* A display projection only. The host retains the complete conversation and execution trace. */
(() => {
  'use strict';
  const root = typeof window === 'undefined' ? globalThis : window;
  const hasText = value => typeof value === 'string' && value.trim().length > 0;
  const ordered = values => (Array.isArray(values) ? values : []).map((value, index) => ({ value, index }))
    .sort((a, b) => (Number.isSafeInteger(a.value.order) ? a.value.order : a.index)
      - (Number.isSafeInteger(b.value.order) ? b.value.order : b.index))
    .map(item => item.value);
  const approval = tool => tool.status === 'approval-required';

  function selectTools(tools, rounds = null) {
    const available = tools.filter(tool => approval(tool) || !rounds || tool.round == null || rounds.has(tool.round));
    const recent = new Set(available.filter(tool => !approval(tool)).slice(-8));
    return available.filter(tool => approval(tool) || recent.has(tool));
  }

  function selectMessagePresentation(message) {
    const source = message || {};
    const content = typeof source.content === 'string' ? source.content : '';
    if (source.role === 'user') return { mode: 'user', segments: [], tools: [], content };
    const segments = ordered(source.assistantSegments), tools = ordered(source.toolActivities);
    const status = source.status || (source.streaming ? 'streaming' : 'completed');
    if (status === 'completed') {
      const final = segments.filter(segment => segment.phase === 'final_answer'
        && segment.status === 'completed' && hasText(segment.content)).at(-1);
      if (final) return { mode: 'final', segments: [final], tools: [], content: final.content };
      if (!segments.length && hasText(content)) return { mode: 'final', segments: [], tools: [], content };
    }
    if (status !== 'streaming') {
      const partial = segments.filter(segment => hasText(segment.content)).at(-1);
      return { mode: status === 'completed' ? 'incomplete' : 'partial',
        segments: partial ? [partial] : [], tools: tools.filter(approval), content: partial?.content || (segments.length ? '' : content) };
    }
    if (!segments.length) return { mode: 'active', segments: [], tools: selectTools(tools), content };
    const last = segments.at(-1), lastStreaming = last?.status === 'streaming' ? last : null;
    const requiredRounds = new Set(tools.filter(approval).map(tool => tool.round));
    const candidates = segments.filter(segment => hasText(segment.content)
      || tools.some(tool => tool.round === segment.round) || segment === lastStreaming);
    const recent = new Set(candidates.slice(-3));
    const visible = segments.filter(segment => recent.has(segment) || requiredRounds.has(segment.round));
    const selectedTools = selectTools(tools, new Set(visible.map(segment => segment.round)));
    // A round that held only an old observation need not leave an empty spacer after tool budgeting.
    const selectedSegments = visible.filter(segment => hasText(segment.content)
      || selectedTools.some(tool => tool.round === segment.round) || segment === lastStreaming);
    return { mode: 'active', segments: selectedSegments, tools: selectedTools,
      content: selectedSegments.map(segment => segment.content || '').filter(Boolean).join('\n\n') };
  }

  function elapsedText(durationMs, strings) {
    if (!Number.isSafeInteger(durationMs) || durationMs <= 0) return '';
    const seconds = Math.ceil(durationMs / 1000);
    const hours = Math.floor(seconds / 3600), minutes = Math.floor(seconds % 3600 / 60), remainder = seconds % 60;
    const key = hours ? 'elapsedHoursMinutesSeconds' : minutes ? 'elapsedMinutesSeconds' : 'elapsedSeconds';
    const fallback = hours ? '用时 {0}小时{1}分钟{2}秒' : minutes ? '用时 {0}分钟{1}秒' : '用时 {0}秒';
    const values = hours ? [hours, minutes, remainder] : minutes ? [minutes, remainder] : [seconds];
    return (strings?.[key] || fallback).replace(/\{(\d+)\}/g, (_, index) => String(values[Number(index)] ?? ''));
  }

  root.KynxaMessagePresentation = Object.freeze({ selectMessagePresentation, elapsedText });
})();
