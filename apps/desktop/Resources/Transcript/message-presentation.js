/* A display projection only. The host retains the complete conversation and execution trace.
 * 仅生成展示投影；宿主保留完整聊天与执行轨迹。 */
(() => {
  'use strict';
  const globalScope = typeof window === 'undefined' ? globalThis : window;
  const hasText = value => typeof value === 'string' && value.trim().length > 0;
  const orderTimelineRecords = values => (Array.isArray(values) ? values : []).map((value, index) => ({ value, index }))
    .sort((left, right) => (Number.isSafeInteger(left.value.order) ? left.value.order : left.index)
      - (Number.isSafeInteger(right.value.order) ? right.value.order : right.index))
    .map(item => item.value);
  const requiresApproval = tool => tool.status === 'approval-required';

  function selectTools(tools) {
    const recentToolActivities = new Set(tools.filter(tool => !requiresApproval(tool)).slice(-8));
    return tools.filter(tool => requiresApproval(tool) || recentToolActivities.has(tool));
  }

  function selectMessagePresentation(message) {
    const source = message || {};
    const content = typeof source.content === 'string' ? source.content : '';
    if (source.role === 'user') return { mode: 'user', segments: [], tools: [], content };
    const segments = orderTimelineRecords(source.assistantSegments), tools = orderTimelineRecords(source.toolActivities);
    const status = source.status || (source.streaming ? 'streaming' : 'completed');
    if (status === 'completed') {
      const finalAnswerSegment = segments.filter(segment => segment.phase === 'final_answer'
        && segment.status === 'completed' && hasText(segment.content)).at(-1);
      if (finalAnswerSegment) return { mode: 'final', segments: [finalAnswerSegment], tools: [], content: finalAnswerSegment.content };
      if (!segments.length && hasText(content)) return { mode: 'final', segments: [], tools: [], content };
    }
    const mode = status === 'streaming' ? 'active' : status === 'completed' ? 'incomplete' : 'partial';
    const visibleTools = selectTools(tools);
    if (!segments.length) return { mode, segments: [], tools: visibleTools, content };
    const lastSegment = segments.at(-1), lastStreamingSegment = mode === 'active' && lastSegment?.status === 'streaming' ? lastSegment : null;
    const toolRounds = new Set(visibleTools.map(tool => tool.round));
    // Keep every spoken stage until the request has a completed final answer.
    // 请求的最终回答完成之前，保留所有已输出的中间正文。
    const visibleSegments = segments.filter(segment => hasText(segment.content)
      || toolRounds.has(segment.round) || segment === lastStreamingSegment);
    return { mode, segments: visibleSegments, tools: visibleTools,
      content: visibleSegments.map(segment => segment.content || '').filter(hasText).join('\n\n') };
  }

  function elapsedText(durationMs, strings) {
    if (!Number.isSafeInteger(durationMs) || durationMs <= 0) return '';
    const seconds = Math.ceil(durationMs / 1000);
    const hours = Math.floor(seconds / 3600), minutes = Math.floor(seconds % 3600 / 60), remainingSeconds = seconds % 60;
    const key = hours ? 'elapsedHoursMinutesSeconds' : minutes ? 'elapsedMinutesSeconds' : 'elapsedSeconds';
    const fallback = hours ? '用时 {0}小时{1}分钟{2}秒' : minutes ? '用时 {0}分钟{1}秒' : '用时 {0}秒';
    const values = hours ? [hours, minutes, remainingSeconds] : minutes ? [minutes, remainingSeconds] : [seconds];
    return (strings?.[key] || fallback).replace(/\{(\d+)\}/g, (_, index) => String(values[Number(index)] ?? ''));
  }

  globalScope.KynxaMessagePresentation = Object.freeze({ selectMessagePresentation, elapsedText });
})();
