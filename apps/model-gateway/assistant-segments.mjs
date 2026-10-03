import { randomUUID } from 'node:crypto';

const phases = new Set(['commentary', 'final_answer']);
const statuses = new Set(['streaming', 'completed', 'interrupted']);

/** Only public text enters the transcript; provider signatures remain in protocol continuations. */
export function validateAssistantSegments(value) {
  const invalid = () => Object.assign(new Error('助手消息段格式无效，原记录已保留。'),
    { code: 'INVALID_CONVERSATION_DATA', statusCode: 400 });
  if (!Array.isArray(value) || value.length > 128) throw invalid();
  const ids = new Set(), orders = new Set();
  let previousOrder = -1, previousRound = 0, finalSeen = false;
  return value.map(segment => {
    if (!segment || typeof segment !== 'object' || Array.isArray(segment) ||
        typeof segment.id !== 'string' || !/^[a-zA-Z0-9_-]{1,128}$/.test(segment.id) || ids.has(segment.id) ||
        !Number.isSafeInteger(segment.round) || segment.round < 1 || segment.round > 128 || segment.round <= previousRound ||
        !Number.isSafeInteger(segment.order) || segment.order < 0 || segment.order > 1024 || orders.has(segment.order) ||
        segment.order <= previousOrder || finalSeen || !phases.has(segment.phase) || !statuses.has(segment.status) ||
        typeof segment.content !== 'string' || typeof segment.reasoning !== 'string' ||
        segment.content.length + segment.reasoning.length > 2 * 1024 * 1024 ||
        !Number.isSafeInteger(segment.reasoningDurationMs ?? 0) || (segment.reasoningDurationMs ?? 0) < 0) throw invalid();
    ids.add(segment.id); orders.add(segment.order);
    previousOrder = segment.order; previousRound = segment.round;
    finalSeen = segment.phase === 'final_answer';
    return { id: segment.id, round: segment.round, order: segment.order, phase: segment.phase,
      status: segment.status, content: segment.content, reasoning: segment.reasoning,
      reasoningDurationMs: segment.reasoningDurationMs ?? 0 };
  });
}

/** Projects tagged stream events onto the durable public message, without duplicating tool results. */
export function applyAssistantSegmentEvent(assistant, event) {
  if (event.type === 'assistant_segment') {
    const items = (assistant.AssistantSegments ?? []).filter(item => item.id !== event.segment.id);
    assistant.AssistantSegments = [...items, { ...event.segment }].sort((a, b) => a.order - b.order);
    return true;
  }
  if (!event.segmentId) return false;
  const index = assistant.AssistantSegments?.findIndex(item => item.id === event.segmentId) ?? -1;
  if (index < 0) throw Object.assign(new Error('助手消息段顺序无效。'), { code: 'INVALID_ASSISTANT_SEGMENT' });
  const items = [...assistant.AssistantSegments], segment = { ...items[index] };
  if (event.type === 'text_delta') segment.content += event.delta;
  if (event.type === 'reasoning_delta') segment.reasoning += event.delta;
  if (event.type === 'content_snapshot') { segment.content = event.content; segment.reasoning = event.reasoning; }
  items[index] = segment; assistant.AssistantSegments = items;
  return true;
}

export function assistantSegmentText(assistant, field = 'content') {
  return (assistant.AssistantSegments ?? []).map(segment => segment[field]).filter(Boolean).join('\n\n');
}

/** One segment per model round, interleaved with tool orders from the same monotonically increasing counter. */
export class AssistantSegments {
  constructor(emit) { this.emit = emit; this.items = []; this.order = 0; }

  start(round) {
    this.current = { id: randomUUID(), round, order: this.order++, phase: 'commentary',
      status: 'streaming', content: '', reasoning: '', reasoningDurationMs: 0 };
    this.items.push(this.current);
    this.thinkingStarted = undefined;
    this.publish();
  }

  receive(event) {
    const segment = this.current;
    if (event.type === 'text_delta') { this.stopThinking(); segment.content += event.delta; }
    if (event.type === 'reasoning_delta') {
      this.thinkingStarted ??= Date.now(); segment.reasoning += event.delta;
    }
    if (event.type === 'content_snapshot') {
      segment.content = event.content; segment.reasoning = event.reasoning;
    }
    this.emit({ ...event, segmentId: segment.id });
  }

  finish(turn) {
    this.stopThinking();
    Object.assign(this.current, { content: turn.content, reasoning: turn.reasoning,
      phase: turn.calls.length ? 'commentary' : 'final_answer', status: 'completed' });
    this.publish();
  }

  interrupt() {
    if (this.current?.status !== 'streaming') return;
    this.stopThinking(); this.current.status = 'interrupted'; this.publish();
  }

  stopThinking() {
    if (this.thinkingStarted === undefined) return;
    this.current.reasoningDurationMs += Date.now() - this.thinkingStarted;
    this.thinkingStarted = undefined;
  }

  publish() { this.emit({ type: 'assistant_segment', segment: { ...this.current }, toolStreamProtocol: 3 }); }
  snapshot() { return this.items.map(item => ({ ...item })); }
  text() { return this.items.map(item => item.content).filter(Boolean).join('\n\n'); }
  reasoning() { return this.items.map(item => item.reasoning).filter(Boolean).join('\n\n'); }
}
