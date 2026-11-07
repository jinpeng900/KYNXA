import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../../apps/desktop/Resources/Transcript/message-presentation.js', import.meta.url), 'utf8');
const context = vm.createContext({});
vm.runInContext(source, context);
const { mergeMessagePatch, selectMessagePresentation } = context.KynxaMessagePresentation;
const content = '# 长回复\n\n' + '正文与格式保持不变。'.repeat(20000);
const body = { id: 'reply', role: 'assistant', content, html: '<p>' + content + '</p>', reasoningHtml: '',
  assistantSegments: [{ id: 'answer', round: 1, order: 0, phase: 'commentary', status: 'completed', content, html: '<p>' + content + '</p>' }],
  status: 'streaming', toolActivities: [] };
const patch = { id: 'reply', role: 'assistant', reuseBody: true, content: null, html: null, reasoningHtml: null,
  assistantSegments: null, status: 'streaming', executionPhase: 'waiting-resource',
  toolActivities: [{ toolCallId: 'one', status: 'completed', name: 'filesystem.write', round: 1, order: 1 }] };
const merged = mergeMessagePatch(body, patch);
assert.equal(merged.content, content);
assert.equal(merged.html, body.html);
assert.equal(merged.assistantSegments, body.assistantSegments);
assert.equal(merged.executionPhase, 'waiting-resource');
assert.equal(merged.toolActivities[0].toolCallId, 'one');
assert.equal(body.toolActivities.length, 0);
assert.equal(mergeMessagePatch(null, patch), null);
assert.equal(mergeMessagePatch({ ...body, id: 'other' }, patch), null);
const final = { ...body, status: 'completed', content: '最终结果', assistantSegments: [
  { ...body.assistantSegments[0], phase: 'final_answer', content: '最终结果' }] };
assert.equal(mergeMessagePatch(body, final), final);
assert.equal(selectMessagePresentation(final).content, '最终结果');
// Deferred selection patches use the most recent source; stale DOM text must not replace it.
// 选区延迟期间从最新待发布正文合并，不能用旧 DOM 文本覆盖新输出。
const newer = { ...body, content: content + '新增正文' };
assert.equal(mergeMessagePatch(newer, patch).content, newer.content);
const fullBytes = Buffer.byteLength(JSON.stringify({ ...body, ...patch, reuseBody: false,
  content: body.content, html: body.html, assistantSegments: body.assistantSegments }));
const patchBytes = Buffer.byteLength(JSON.stringify(patch));
assert.ok(patchBytes < 1000 && patchBytes < fullBytes / 100);
console.log(JSON.stringify({ passed: 12, fullBytes, patchBytes, unchangedTextCharacters: content.length }));
