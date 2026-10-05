import assert from 'node:assert/strict';
import { test } from 'node:test';
import { VERIFIER_VERSION, assessReviewerRelations, readUsage, summarizeRuns, verifyTask } from './evaluation.mjs';
import { rescoreResults } from './rescore.mjs';

test('v3 shared negation in respective pairs denies the gold in either name order', () => {
  for (const answer of [
    'ORION 和 VEGA 的审查人分别不是 Mara Chen 和 Beatrice Hall。',
    'ORION 和 VEGA 的审查人分别并非 Mara Chen 和 Beatrice Hall。',
    'ORION and VEGA are not reviewed by Mara Chen and Beatrice Hall respectively.',
    'Mara Chen and Beatrice Hall are not the reviewers of ORION and VEGA respectively.'
  ]) {
    const assessment = assessReviewerRelations(answer);
    assert.equal(assessment.entityRetained, false, answer);
    assert.ok(assessment.relations.some(relation => relation.entity === 'ORION' && relation.negated), answer);
  }
});

test('v3 contradictory affirmation and denial of the gold is rejected in either order', () => {
  for (const answer of [
    'ORION 的审查人是 Mara Chen。ORION 的审查人不是 Mara Chen。',
    'ORION 的审查人不是 Mara Chen。ORION 的审查人是 Mara Chen。',
    'Mara Chen is the reviewer for ORION. Mara Chen is not the reviewer for ORION.',
    'ORION is reviewed by Mara Chen. ORION is not reviewed by Mara Chen.'
  ]) {
    const assessment = assessReviewerRelations(answer);
    assert.equal(assessment.entityRetained, false, answer); assert.equal(assessment.contradictoryGold, true, answer);
  }
});

test('v3 retains correct contrast, respective positive pairs and denial of the wrong reviewer', () => {
  for (const answer of [
    'ORION 的审查人是 Mara Chen。VEGA 的审查人是 Beatrice Hall。',
    'ORION 和 VEGA 的审查人分别是 Mara Chen 和 Beatrice Hall。',
    'Mara Chen and Beatrice Hall are reviewers of ORION and VEGA respectively.',
    'ORION 的审查人不是 Beatrice Hall 而是 Mara Chen。',
    'ORION 的审查人是 Mara Chen。ORION 的审查人不是 Beatrice Hall。'
  ]) assert.equal(assessReviewerRelations(answer).entityRetained, true, answer);
  for (const answer of [
    'ORION 的审查人是 Beatrice Hall。VEGA 的审查人是 Mara Chen。',
    'ORION 和 VEGA 的审查人分别是 Beatrice Hall 和 Mara Chen。',
    'ORION 的审查人是 Mara Chen。ORION 的审查人是 Beatrice Hall。'
  ]) assert.equal(assessReviewerRelations(answer).entityRetained, false, answer);
  assert.equal(VERIFIER_VERSION, 'agent-verifier-reviewer-relations-v3');
});

test('v3 negated launch status does not negate a separately affirmed reviewer', () => {
  for (const answer of [
    'ORION is not a confirmed launch. Its safety reviewer is Mara Chen.',
    'ORION is not a confirmed launch and its safety reviewer is Mara Chen.',
    'ORION 的窗口不是已确认发射。其安全审查人是 Mara Chen。',
    'ORION 的窗口不是已确认发射且其安全审查人是 Mara Chen。'
  ]) assert.equal(assessReviewerRelations(answer).entityRetained, true, answer);
});

test('v3 reviewer changes leave date facts and the no-date output contract intact', () => {
  const assistant = { Status: 'completed', Content: 'ORION 的实验窗口为 2031 年 11 月 14 日 17:40 UTC，'
    + '安全审查人 Mara Chen。尚未确认发射。来源 fixture-notes.md。', EvidenceReferences: [{ sourceId: 'fixed-source' }] };
  assert.equal(verifyTask('english-source-chinese-answer', { assistants: [assistant], sourceId: 'fixed-source' }).success, true);
  assert.equal(verifyTask('english-source-chinese-answer', { assistants: [{ ...assistant, Content: assistant.Content.replace('2031', '2032') }],
    sourceId: 'fixed-source' }).success, false);
  assert.equal(verifyTask('insufficient-evidence', { assistants: [{ Status: 'completed', Content: '资料不足，无法确定 NOVA 的发射时间。' }] }).success, true);
  assert.equal(verifyTask('insufficient-evidence', { assistants: [{ Status: 'completed', Content: '无法确定，但可能在 2031 年。' }] }).success, false);
});

test('v3 explicit headers bind continuous reviewer bullets and switch to the next named entity', () => {
  const accepted = [
    '## ORION\n- 日期：2031年11月14日\n- UTC时间：17:40 UTC\n- 安全审查人：Mara Chen\n\n## VEGA\n- 安全审查人：Beatrice Hall',
    '- 项目：ORION\n- 时间：17:40 UTC\n- 审查人：Mara Chen\n- 项目：VEGA\n- 审查人：Beatrice Hall',
    '| 项目 | 审查人 |\n| ORION | Mara Chen |\n| VEGA | Beatrice Hall |',
    '| 项目 | ORION | VEGA |\n| 审查人 | Mara Chen | Beatrice Hall |',
    'ORION 的日期为2031年11月14日，安全审查人是 Mara Chen。VEGA 的安全审查人是 Beatrice Hall。'
  ];
  const rejected = [
    '## ORION\n- 审查人：Beatrice Hall\n\n## VEGA\n- 审查人：Mara Chen',
    '- 项目：ORION\n- 审查人：Beatrice Hall\n- 项目：VEGA\n- 审查人：Mara Chen',
    '| 项目 | 审查人 |\n| ORION | Beatrice Hall |\n| VEGA | Mara Chen |',
    '| 项目 | ORION | VEGA |\n| 审查人 | Beatrice Hall | Mara Chen |',
    '## ORION\n- 审查人：Mara Chen\n- 审查人不是 Mara Chen',
    'ORION。\n\n审查人：Mara Chen',
    '日期为2031年11月14日，安全审查人是 Mara Chen。'
  ];
  for (const answer of accepted) assert.equal(assessReviewerRelations(answer).entityRetained, true, answer);
  for (const answer of rejected) assert.equal(assessReviewerRelations(answer).entityRetained, false, answer);
});

test('v3 QA gold rejects globally present but wrongly bound reviewers and preserves the required ORION name', () => {
  const base = 'ORION 实验窗口：2031年11月14日17:40 UTC，尚未确认发射。\n';
  const gold = { Status: 'completed', EvidenceReferences: [{ sourceId: 'fixed-source' }],
    Content: base + '- 审查人：Mara Chen\n\nVEGA\n- 审查人：Beatrice Hall\n来源：fixture-notes.md。' };
  assert.equal(verifyTask('english-source-chinese-answer', { assistants: [gold], sourceId: 'fixed-source' }).success, true);
  const wrong = { ...gold, Content: base + '- 审查人：Beatrice Hall\n\nVEGA\n- 审查人：Mara Chen\n来源：fixture-notes.md。' };
  assert.equal(verifyTask('english-source-chinese-answer', { assistants: [wrong], sourceId: 'fixed-source' }).checks.goldFacts, false);
  const unnamed = { ...gold, Content: '实验窗口：2031年11月14日17:40 UTC，尚未确认发射。安全审查人：Mara Chen。来源：fixture-notes.md。' };
  assert.equal(verifyTask('english-source-chinese-answer', { assistants: [unnamed], sourceId: 'fixed-source' }).checks.goldFacts, false);
});

test('v3 rescoring recomputes repetitions and totals consistently while preserving cost and source identity', () => {
  const call = { durationMs: 5, usage: readUsage('openai-completions', { usage: { prompt_tokens: 10, completion_tokens: 2 } }) };
  const common = { executionStatus: 'completed', modelCalls: [call], toolCalls: 0, durationMs: 5, setupMs: 1 };
  const followup = { ...common, taskId: 'followup-entity', success: false,
    checks: { completed: true, twoTurns: true, initialFacts: true, entityRetained: false, groundedCitation: true },
    answers: [{ content: 'ORION 的审查人是 Mara Chen；VEGA 的审查人是 Beatrice Hall。' }] };
  const qa = { ...common, taskId: 'english-source-chinese-answer', success: true,
    checks: { completed: true, chineseAnswer: true, goldFacts: true, experimentalOnly: true, groundedCitation: true },
    answers: [{ content: 'ORION 的安全审查人是 Mara Chen。' }] };
  const runs = [{ ...followup, configuration: 'rag-off', repetition: 1 }, { ...qa, configuration: 'rag-on', repetition: 1 },
    { ...followup, configuration: 'rag-on', repetition: 2 }, { ...qa, configuration: 'rag-off', repetition: 2, success: false,
      checks: { ...qa.checks, goldFacts: false }, answers: [{ content: '安全审查人是 Mara Chen。' }] }];
  const original = { schemaVersion: 1, sourceFingerprint: { combinedHash: 'fixed-original-source' }, runs,
    summary: summarizeRuns(runs), configurations: { 'rag-off': summarizeRuns(runs.filter(run => run.configuration === 'rag-off')),
      'rag-on': summarizeRuns(runs.filter(run => run.configuration === 'rag-on')) },
    repetitions: { 1: { summary: summarizeRuns(runs.slice(0, 2)) }, 2: { summary: summarizeRuns(runs.slice(2)) } } };
  const result = rescoreResults(original);
  assert.equal(result.summary.successful, 3);
  assert.equal(result.repetitions[1].summary.successful, 2); assert.equal(result.repetitions[2].summary.successful, 1);
  assert.equal(result.summary.successful, Object.values(result.repetitions).reduce((sum, repetition) => sum + repetition.summary.successful, 0));
  assert.equal(result.configurations['rag-off'].successful, 1); assert.equal(result.configurations['rag-on'].successful, 2);
  assert.equal(result.runs[3].checks.goldFacts, false); assert.equal(result.runs[3].success, false);
  assert.deepEqual(result.summary.allCost, original.summary.allCost); assert.deepEqual(result.sourceFingerprint, original.sourceFingerprint);
  assert.equal(result.rescoring.newModelCalls, 0); assert.equal(original.runs[0].checks.entityRetained, false);
});
