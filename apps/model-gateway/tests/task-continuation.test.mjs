import assert from 'node:assert/strict';
import { test } from 'node:test';
import { TaskContinuation, TaskTextProjection } from '../orchestration/task-continuation.mjs';
import { runToolLoop } from '../orchestration/tool-loop.mjs';
import { randomUUID } from 'node:crypto';

test('an explicit final continuation footer is stripped and exposes the next action', () => {
  const continuation = new TaskContinuation();
  const draft = '已完成第一部分。\n<kynxa_task>{"state":"continue","next":"write next section"}</kynxa_task>';
  const result = continuation.inspect(draft);
  assert.equal(result.state, 'continue');
  assert.equal(result.next, 'write next section');
  assert.equal(result.content.trimEnd(), '已完成第一部分。');
  assert.ok(!result.content.includes('<kynxa_task>'));
});

test('complete and blocked footers preserve the public answer and do not request another action', () => {
  const continuation = new TaskContinuation();
  for (const state of ['complete', 'blocked']) {
    const draft = `公开回答。\n<kynxa_task>{"state":"${state}","reason":"explicit outcome"}</kynxa_task>`;
    const result = continuation.inspect(draft);
    assert.equal(result.state, state);
    assert.equal(result.content.trimEnd(), '公开回答。');
    assert.equal(result.reason, 'explicit outcome');
    assert.ok(!result.next);
  }
});

test('ordinary legacy answers and greetings remain unchanged and do not imply continuation', () => {
  const continuation = new TaskContinuation();
  for (const draft of ['你好！', '这段函数负责取消任务。\n', '答案是42。']) {
    const result = continuation.inspect(draft);
    assert.equal(result.state, 'unknown');
    assert.equal(result.content, draft);
    assert.ok(!result.next);
  }
});

test('malformed, unsupported, and non-final footers are retained as ordinary content', () => {
  const continuation = new TaskContinuation();
  for (const draft of [
    '正文。\n<kynxa_task>{broken json}</kynxa_task>',
    '正文。\n<kynxa_task>{"state":"invented"}</kynxa_task>',
    '正文。\n<kynxa_task>{"state":"continue","next":"next"}</kynxa_task>\n额外正文。',
    '{"tool":"read","content":"<kynxa_task>{\\"state\\":\\"continue\\"}</kynxa_task>"}'
  ]) {
    const result = continuation.inspect(draft);
    assert.equal(result.state, 'unknown', draft);
    assert.equal(result.content, draft);
  }
});

test('quoted and fenced footer examples cannot become task control metadata', () => {
  const continuation = new TaskContinuation();
  const footer = '<kynxa_task>{"state":"continue","next":"write next section"}</kynxa_task>';
  for (const draft of [
    `示例：\n\`\`\`text\n${footer}\n\`\`\``,
    `示例：\n\`\`\`text\n${footer}`,
    `引用：\n> ${footer}`
  ]) {
    const result = continuation.inspect(draft);
    assert.equal(result.state, 'unknown', draft);
    assert.equal(result.content, draft);
  }
});

test('a narrow final handoff paragraph requests review rather than confirmed continuation', () => {
  const continuation = new TaskContinuation();
  const draft = '第五章正文。\n\n下一段将继续从第六章写起，说明量子测量的含义。';
  const result = continuation.inspect(draft);
  assert.equal(result.state, 'review');
  assert.equal(result.content, draft);
  const contradictory = continuation.inspect(draft + '\n<kynxa_task>{"state":"complete"}</kynxa_task>');
  assert.equal(contradictory.state, 'review'); assert.equal(contradictory.source, 'conflicting-completion-cue');
});

test('recommendations, quotations, and fenced handoff examples do not request review', () => {
  const continuation = new TaskContinuation();
  for (const draft of [
    '下一步建议：先阅读第六章，再整理笔记。',
    '引用：\n> 下一段将继续从第六章写起。',
    '示例：\n```text\n下一段将继续从第六章写起。\n```'
  ]) {
    const result = continuation.inspect(draft);
    assert.equal(result.state, 'unknown', draft);
    assert.equal(result.content, draft);
  }
});

function archive() {
  return { save: async () => ({ id: randomUUID(), bytes: 200, sha256: 'a'.repeat(64) }) };
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages']) {
  test(`${protocol} model-declared unfinished work continues in one reply without counting words`, async () => {
    const events = [], states = []; let requests = 0;
    const result = await runToolLoop({ protocol, messages: [{ role: 'user', content: '完成这个任务。' }],
      declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat', message: '完成这个任务。' },
      service: { results: archive(), execute: () => assert.fail('text status never grants tools') },
      emit: event => events.push(event), saveActivity: async () => {}, saveRunState: async state => states.push(state),
      requestTurn: async (messages, _declarations, _signal, receive) => {
        requests++;
        const content = messages.at(-1)?.content?.includes('[KYNXA_TASK_COMPLETION_REVIEW]')
          ? '<kynxa_task>{"state":"complete","reason":"原始要求的两部分正文已经全部交付"}</kynxa_task>'
          : requests === 1 ? '第一部分正文。\n<kynxa_task>{"state":"continue","next":"完成第二部分"}</kynxa_task>'
          : '第二部分正文。\n<kynxa_task>{"state":"complete"}</kynxa_task>';
        if (requests === 2) assert.ok(messages.at(-1).content.includes('You declared the original task unfinished'));
        for (const delta of content) receive({ type: 'text_delta', delta });
        return { content, reasoning: '', calls: [], finish: protocol === 'anthropic-messages' ? 'end_turn' : 'stop' };
      } });
    assert.equal(requests, 3); assert.equal(result.content, '第一部分正文。\n\n第二部分正文。');
    assert.ok(events.filter(event => event.type === 'text_delta').every(event => !event.delta.includes('kynxa_task')));
    assert.ok(events.filter(event => event.type === 'assistant_segment').every(event => !event.segment.content.includes('kynxa_task')));
    assert.equal(result.taskCompletion.modelTaskState.state, 'complete');
    assert.equal(states.at(-1).taskState.resumes, 1);
    assert.equal(states.at(-1).diagnostics.executedToolCalls, 0);
  });
}

test('a dangling handoff is reviewed by the next model turn, which can finish rather than loop', async () => {
  let requests = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: '解释这个过程。' }],
    declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat' }, service: { results: archive() },
    emit: () => {}, saveActivity: async () => {}, requestTurn: async messages => {
      requests++;
      if (requests === 2) assert.ok(messages.at(-1).content.includes('This cue is not proof of incompleteness'));
      if (messages.at(-1)?.content?.includes('[KYNXA_TASK_COMPLETION_REVIEW]'))
        return { content: '<kynxa_task>{"state":"complete","reason":"现有解释与补充说明共同完成原始要求"}</kynxa_task>', reasoning: '', calls: [], finish: 'stop' };
      return { content: requests === 1 ? '现有解释。\n\n下一段将继续完成其余说明。'
        : '补充说明。\n<kynxa_task>{"state":"complete"}</kynxa_task>', reasoning: '', calls: [], finish: 'stop' };
    } });
  assert.equal(requests, 3); assert.ok(result.content.includes('补充说明。'));
});

test('repeated no-progress continuation returns a preserved partial answer within the run budget', async () => {
  let requests = 0;
  const text = '相同的已有说明。'.repeat(20);
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: '完成任务。' }],
    declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat' }, service: { results: archive() },
    emit: () => {}, saveActivity: async () => {}, requestTurn: async () => {
      requests++; return { content: text + '\n<kynxa_task>{"state":"continue","next":"继续处理"}</kynxa_task>',
        reasoning: '', calls: [], finish: 'stop' };
    } });
  assert.equal(requests, 3); assert.equal(result.completionStatus, 'interrupted');
  assert.equal(result.content.split(text).length - 1, 1);
  assert.equal(result.taskCompletion.modelTaskState.noProgressRounds, 2);
});

test('blocked state explains the limitation without another call or claiming completion', async () => {
  let requests = 0;
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: '处理任务。' }],
    declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat' }, service: {},
    emit: () => {}, saveActivity: async () => {}, requestTurn: async () => {
      requests++; return { content: '已有部分结果。\n<kynxa_task>{"state":"blocked","reason":"缺少指定资料"}</kynxa_task>',
        reasoning: '', calls: [], finish: 'stop' };
    } });
  assert.equal(requests, 1); assert.equal(result.completionStatus, 'interrupted');
  assert.ok(result.content.includes('缺少指定资料')); assert.ok(!result.content.includes('kynxa_task'));
});

for (const nativeLimit of [false, true]) test(`a ${nativeLimit ? 'native output-limit' : 'task'} continuation reuses a completed effect receipt instead of dispatching the same write again`, async () => {
  let requests = 0, executions = 0;
  const store = { ...archive(), get: async () => ({ content: 'Saved.' }),
    modelResult: async () => JSON.stringify({ status: 'completed', output: 'Saved.' }) };
  const result = await runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: '完成说明。' }],
    declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat', message: '完成说明。' },
    service: { results: store, execute: async () => { executions++; return { status: 'completed', content: 'Saved.',
      resultRef: { id: randomUUID(), bytes: 100, sha256: 'b'.repeat(64) } }; } },
    emit: () => {}, saveActivity: async () => {}, requestTurn: async messages => {
      requests++;
      if (messages.at(-1)?.content?.includes('[KYNXA_TASK_COMPLETION_REVIEW]'))
        return { content: '<kynxa_task>{"state":"complete","reason":"说明已完成且保留已成功保存的回执"}</kynxa_task>', reasoning: '', calls: [], finish: 'stop' };
      if (requests === 2) return { content: '已保存文件，继续说明。' + (nativeLimit ? ''
        : '\n<kynxa_task>{"state":"continue","next":"完成说明"}</kynxa_task>'),
        reasoning: '', calls: [], finish: nativeLimit ? 'length' : 'stop', outputTruncated: nativeLimit };
      if (requests === 4) return { content: '说明已经完成。\n<kynxa_task>{"state":"complete"}</kynxa_task>', reasoning: '', calls: [], finish: 'stop' };
      const id = `write-${requests}`, args = { path: 'note.txt', content: 'Requested body.', expectedHash: null };
      return { content: '', reasoning: '', calls: [{ id, name: 'filesystem.write', arguments: args }], finish: 'tool_calls',
        continuation: [{ role: 'assistant', content: '', tool_calls: [{ id, type: 'function',
          function: { name: 'synthetic_write', arguments: JSON.stringify(args) } }] }] };
    } });
  assert.equal(requests, 5); assert.equal(executions, 1); assert.ok(result.content.includes('说明已经完成。'));
});

test('cancelling an unfinished task prevents the next model invocation', async () => {
  const controller = new AbortController(); let requests = 0;
  await assert.rejects(runToolLoop({ protocol: 'openai-completions', messages: [{ role: 'user', content: '处理任务。' }],
    declarations: [], inputBudgetTokens: 4000, context: { conversationId: 'chat' }, service: { results: archive() },
    signal: controller.signal, emit: () => {}, saveActivity: async () => {}, requestTurn: async () => {
      requests++; controller.abort(); return { content: '部分结果。\n<kynxa_task>{"state":"continue","next":"继续"}</kynxa_task>',
        reasoning: '', calls: [], finish: 'stop' };
    } }), { name: 'AbortError' });
  assert.equal(requests, 1);
});

test('stream projection preserves code examples and non-final markers but hides the actual final footer', () => {
  for (const content of ['示例：\n```text\n<kynxa_task>{"state":"complete"}</kynxa_task>\n```\n继续正文。',
    '示例：\n<kynxa_task>{"state":"complete"}</kynxa_task>\n后续正常正文。',
    '正常正文。\n<kynxa_task>{"state":"complete"}</kynxa_task>']) {
    const events = [], projection = new TaskTextProjection(event => events.push(event));
    for (const delta of content) projection.push({ type: 'text_delta', delta });
    const parsed = new TaskContinuation().inspect(content);
    projection.finish(parsed.content, '');
    const snapshot = events.findLast(event => event.type === 'content_snapshot');
    assert.equal(snapshot?.content ?? events.map(event => event.delta ?? '').join(''), parsed.content);
    if (content.includes('```')) assert.ok(events.map(event => event.delta ?? '').join('').includes('继续正文。'));
  }
});
