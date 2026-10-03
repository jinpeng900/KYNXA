/** Elapsed generation time has one gateway owner and uses a monotonic clock. */
export function replyDurationMs(startedAtMonotonicMs) {
  return Math.max(0, Math.ceil(performance.now() - startedAtMonotonicMs));
}

export function storedReplyDurationMs(message) {
  if (message.DurationMs !== undefined && message.DurationMs !== null) {
    if (!Number.isSafeInteger(message.DurationMs) || message.DurationMs < 0)
      throw Object.assign(new Error('回复耗时格式无效，原记录已保留。'), { code: 'INVALID_CONVERSATION_DATA', statusCode: 400 });
    return message.DurationMs;
  }
  // Old terminal tool runs have real start/end receipts. Never derive a duration from the current clock.
  const run = message.ToolRun;
  const status = message.Status ?? 'completed';
  if (message.Role !== 'assistant' || !['completed', 'error', 'interrupted'].includes(status) ||
      run?.version !== 1 || run.phase !== status || typeof run.startedAt !== 'string' || typeof run.updatedAt !== 'string') return 0;
  const start = Date.parse(run.startedAt), end = Date.parse(run.updatedAt), duration = end - start;
  return Number.isSafeInteger(duration) && duration >= 0 ? duration : 0;
}
