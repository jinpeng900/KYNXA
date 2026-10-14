// Exercise the production projection without a DOM, model request or private user data.
// 直接验证正式展示投影，不依赖 DOM、模型调用或真实用户数据；浏览器交互验收仍单独执行。
import assert from 'node:assert/strict';
import '../../apps/desktop/Resources/Transcript/message-presentation.js';

const { selectMessagePresentation: select, elapsedText } = globalThis.KynxaMessagePresentation;
let checks = 0;
function check(condition, description) {
  assert.ok(condition, description);
  checks++;
}
const stage = { id: 'stage', round: 1, order: 1, phase: 'commentary', status: 'completed', content: 'Progress only.' };
const final = { id: 'final', round: 2, order: 3, phase: 'final_answer', status: 'completed', content: 'Answer.' };
const tool = { toolCallId: 'read-fixture', round: 1, order: 2, status: 'completed' };
const settled = { role: 'assistant', status: 'completed', content: 'Stale aggregate.', assistantSegments: [stage, final], toolActivities: [tool] };

check(select({ role: 'user', content: 'User raw.' }).content === 'User raw.', 'User text stays intact.');
check(select({ role: 'assistant', status: 'completed', content: 'Legacy.' }).mode === 'final', 'Legacy replies remain final.');
check(select({ role: 'assistant', status: 'completed', content: ' ' }).mode === 'incomplete', 'Whitespace is not a completed answer.');
check(select(settled).content === 'Answer.', 'Unmarked execution prose stays out of the final answer.');
check(select(settled).tools.length === 0, 'Final answers hide tool display records.');
check(select({ ...settled, status: 'streaming' }).content === 'Progress only.\n\nAnswer.', 'Streaming preserves received prose.');
check(select({ ...settled, status: 'error' }).mode === 'partial', 'Errors preserve a partial display.');
check(select({ ...settled, status: 'interrupted' }).content.includes('Progress only.'), 'Interruptions retain earlier prose.');
check(select({ ...settled, assistantSegments: [stage] }).mode === 'incomplete', 'A completed transport does not invent a final phase.');
for (const completionState of ['partial', 'verification-failed', 'execution-unknown']) {
  check(select({ ...settled, taskCompletion: { completionState } }).content === 'Answer.', 'Task outcomes do not hide delivered final text.');
}
const manyTools = Array.from({ length: 20 }, (_, index) => ({ toolCallId: 'call-' + index, status: 'completed' }));
const approval = { toolCallId: 'approval', status: 'approval-required' };
check(select({ role: 'assistant', status: 'streaming', toolActivities: [approval, ...manyTools] }).tools.length === 9,
  'Older approval survives bounded ordinary tools.');
check(select({ ...settled, assistantSegments: [final, stage] }).content === 'Answer.', 'Timeline sorting is stable.');
check(elapsedText(17000, {}) === '用时 17秒', 'Actual settled duration is projected.');
check(elapsedText(0, {}) === '', 'Unset settled durations remain blank.');
check(elapsedText(1600, {}, { live: true }) === '用时 1秒', 'Live elapsed time does not round ahead.');

const prefix = { ...stage, id: 'prefix', round: 2, order: 2, isAnswerContinuation: true, content: '正文开头\n\n```cs\nint count =' };
const middle = { ...stage, id: 'middle', round: 3, order: 3, isAnswerContinuation: true, content: '2;\n```\n\n$$\\alpha' };
const ending = { ...final, round: 4, order: 4, content: '^2$$\n\n正文结尾。' };
const chunks = [stage, prefix, middle, ending];
const continued = { ...settled, assistantSegments: chunks };
const original = JSON.stringify(chunks);
const completeAnswer = [prefix.content, middle.content, ending.content].join('\n\n');
check(select(continued).content === completeAnswer, 'All explicit output chunks survive final projection and copy.');
check(select(continued).segments.length === 3 && select(continued).tools.length === 0, 'Only the contiguous answer chain is selected.');
check(JSON.stringify(chunks) === original, 'Projection never rewrites formal segments.');
check(select({ ...continued, assistantSegments: [stage, prefix, { ...middle, isAnswerContinuation: false }, ending] }).content === ending.content,
  'An unmarked stage stops the continuation chain.');
check(select({ ...continued, assistantSegments: [stage, prefix, { ...middle, status: 'interrupted' }, ending] }).content === ending.content,
  'An interrupted chunk is not promoted to settled output.');
check(select({ ...continued, assistantSegments: chunks.map(chunk => ({ ...chunk, isAnswerContinuation: undefined })) }).content === ending.content,
  'Older records never infer continuation markers.');
check(select({ ...continued, assistantSegments: [stage, prefix, { ...middle, isAnswerContinuation: 'true' }, ending] }).content === ending.content,
  'A string flag cannot claim continuation authority.');
check(select({ ...continued, taskCompletionState: 'partial' }).content === completeAnswer,
  'Partial task acceptance still delivers the complete continued answer.');
console.log(`Message presentation smoke: ${checks} checks passed.`);
