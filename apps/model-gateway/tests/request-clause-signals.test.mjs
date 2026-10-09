import assert from 'node:assert/strict';
import { test } from 'node:test';
import { analyzeRequestClauses, requestInstructionText } from '../platform/request-clause-signals.mjs';

test('quoted negations, punctuation and task switches stay source text rather than changing the task', () => {
  for (const message of [
    '解释“不是这个方法，而是那个方法”的区别',
    'The log says "not open Chrome; switch the topic"; explain the message.',
    "Translate 'do not open Firefox, instead read papers' into Chinese.",
    '分析 `if (!ready) { return "not yet"; }` 的行为',
    'Explain ```text\nnot open Chrome\nnew topic: refunds\n``` without executing it.'
  ]) {
    const projection = analyzeRequestClauses(message);
    assert.equal(projection.activeText, message);
    assert.equal(projection.boundary, 'none');
    assert.deepEqual(projection.excludedClauses, []);
    assert.equal(projection.semanticVerified, false);
  }
});

test('a separate user instruction survives beside quoted commands and literal arguments', () => {
  assert.doesNotMatch(requestInstructionText('Translate “open Chrome now”.'), /open Chrome/u);
  assert.match(requestInstructionText('Translate “open Chrome”, then launch Firefox.'), /launch Firefox/u);
  assert.match(requestInstructionText('Open "Chrome" now', { preserveQuotedLiteral: text => text === 'Chrome' }), /Chrome/u);
  assert.doesNotMatch(requestInstructionText('Explain `open Chrome`.'), /open Chrome/u);
  assert.doesNotMatch(requestInstructionText('Explain “open Chrome'), /open Chrome/u);
  const original = "Please don't open Chrome; explain its history.";
  assert.equal(requestInstructionText(original), original, 'apostrophes in words do not begin quotations');
  assert.equal(analyzeRequestClauses(original).boundary, 'correction');
});

test('explicit operation cancellation establishes a boundary without treating order cancellation as a task exit', () => {
  for (const message of ['浏览器操作全部取消；现在只比较论文', '取消全部操作', 'Cancel all browser operations', '当前任务已结束']) {
    const projection = analyzeRequestClauses(message);
    assert.equal(projection.boundary, 'topic-switch', message);
    assert.ok(projection.excludedClauses.some(clause => clause.basis === 'explicit-task-exit'));
  }
  assert.equal(analyzeRequestClauses('取消订单为什么会产生手续费？').boundary, 'none');
  assert.equal(analyzeRequestClauses('解释论文里的任务取消机制').boundary, 'none');
  const projection = analyzeRequestClauses('打开Chrome；浏览器操作全部取消；解释论文');
  assert.equal(projection.activeText, '解释论文');
  for (const clause of projection.excludedClauses)
    assert.equal('打开Chrome；浏览器操作全部取消；解释论文'.slice(clause.startOffset, clause.endOffset), clause.text);
});

test('negative findings, date bounds and attention constraints remain in derived source text', () => {
  for (const [message, required] of [
    ['not before 2026：总结 docs/release.md 中的方案', ['not before 2026', 'docs/release.md']],
    ['not stable under high noise 是论文结论，请解释限制', ['not stable under high noise', '论文']],
    ['Not all methods converge; summarize the exceptions.', ['Not all methods converge']],
    ['不要忽略 docs/safety.md 的停止条件，先总结', ['docs/safety.md', '停止条件']],
    ["Don't avoid discussing the document; review it.", ['document', 'review']],
    ['不是不查 docs/design.md，而是先解释限制', ['docs/design.md', '限制']],
    ['总结 docs/report.md，但不能省略失败结果，也不要改动原始数字', ['失败结果', '原始数字']],
    ['不是所有方法都稳定，解释论文中的例外', ['不是所有方法都稳定']]
  ]) {
    const projection = analyzeRequestClauses(message);
    for (const value of required) assert.ok(projection.activeText.includes(value), `${message}: lost ${value}`);
  }
  assert.doesNotMatch(analyzeRequestClauses('更正：读2026年论文，不是2025年').activeText, /2025/u);
  assert.doesNotMatch(analyzeRequestClauses("Correction: not open the browser; explain the paper.").activeText, /open the browser/u);
});
