import assert from 'node:assert/strict';
import { test } from 'node:test';
import { SYSTEM_TASKS, parseFinalObject, verifySystemTask, summarizeSystemChecks } from './system-suite.mjs';

const task = id => SYSTEM_TASKS.find(item => item.id === id);
const answer = (item, extra = {}) => ({ Status: 'completed', Content: JSON.stringify({ ...item.gold, sources: item.sources }), ...extra });
const read = (path, content, round) => ({ name: 'filesystem.read', arguments: { path }, status: 'completed', round,
  result: JSON.stringify({ content }) });

test('greetings require an actual salutation, not merely Chinese text or a substring of This', () => {
  for (const [id, valid, invalid] of [['greeting-zh', '你好！', '这是无关陈述。'], ['greeting-en', 'Hello!', 'This is unrelated.']]) {
    assert.equal(verifySystemTask(task(id), { assistants: [{ Status: 'completed', Content: valid }] }).success, true);
    assert.equal(verifySystemTask(task(id), { assistants: [{ Status: 'completed', Content: invalid }] }).success, false);
  }
});

test('grounded JSON requires matching fields, schema, actual source and observed supporting body', () => {
  const item = task('long-section'), value = answer(item, { ToolActivities: [read('equipment.md', item.files['equipment.md'], 1)] });
  assert.equal(verifySystemTask(item, { assistants: [value] }).success, true);
  value.ToolActivities[0] = read('equipment.md', 'Maintenance entry 1: keep the enclosure clean.', 1);
  assert.equal(verifySystemTask(item, { assistants: [value] }).checks.supportedFacts, false);
  value.ToolActivities = []; value.EvidenceReferences = [{ title: 'equipment.md' }];
  assert.equal(verifySystemTask(item, { assistants: [value] }).checks.citedSources, false);
  value.BenchmarkObservations = [{ title: 'equipment.md', text: item.files['equipment.md'] }];
  assert.equal(verifySystemTask(item, { assistants: [value] }).success, true);
  value.Content = JSON.stringify({ ...item.gold, sources: item.sources, unexpected: 'claim' });
  assert.equal(verifySystemTask(item, { assistants: [value] }).checks.outputSchema, false);
});

test('follow-up attribution and citations cannot borrow observations from the future second turn', () => {
  const item = task('followup-entity');
  const initial = { Status: 'completed', Content: JSON.stringify({ projects: item.firstGold, sources: item.sources }) };
  const final = answer(item, { ToolActivities: [read('launch.md', item.files['launch.md'], 1)] });
  assert.equal(verifySystemTask(item, { assistants: [initial, final] }).checks.firstTurnSources, false);
  initial.BenchmarkObservations = [{ title: 'launch.md', text: item.files['launch.md'] }];
  assert.equal(verifySystemTask(item, { assistants: [initial, final] }).success, true);
  final.Content = JSON.stringify({ ...item.gold, reviewer: 'Emil Reed', sources: item.sources });
  assert.equal(verifySystemTask(item, { assistants: [initial, final] }).checks.factFields, false);
});

test('file cycles require ordered dependent rounds, actual full disk state and a real readback', () => {
  const item = task('dependent-file-cycle'), content = item.effect.content;
  const activities = [read('seed.txt', content, 1), { name: 'filesystem.write', arguments: { path: 'receipt.txt' }, status: 'completed', round: 2 },
    read('receipt.txt', content, 3)];
  const value = answer(item, { ToolActivities: activities });
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'receipt.txt': content } }).success, true);
  activities[2].round = 2;
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'receipt.txt': content } }).checks.dependentToolRounds, false);
  activities[2].round = 3; activities[2].reused = true;
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'receipt.txt': content } }).checks.readBackState, false);
});

test('editing must preserve every JSON key and use the actually read original hash', () => {
  const item = task('conflict-aware-edit'), expected = JSON.stringify(item.effect.expected);
  const activities = [read('config.json', item.files['config.json'], 1),
    { name: 'filesystem.write', arguments: { path: 'config.json', expectedHash: item.effect.originalHash }, status: 'completed', round: 2 },
    read('config.json', expected, 3)];
  const value = answer(item, { ToolActivities: activities });
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'config.json': expected } }).success, true);
  const added = JSON.stringify({ ...item.effect.expected, unexpected: true });
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'config.json': added } }).checks.diskState, false);
  activities[1].arguments.expectedHash = 'wrong';
  assert.equal(verifySystemTask(item, { assistants: [value], disk: { 'config.json': expected } }).checks.conflictHash, false);
});

test('fallback recovery requires a later round after the observed missing-file failure', () => {
  const item = task('recover-missing-file'), activities = [
    { name: 'filesystem.read', arguments: { path: 'missing.md' }, status: 'error', round: 1 },
    read('fallback.md', item.files['fallback.md'], 2)];
  const value = answer(item, { ToolActivities: activities });
  assert.equal(verifySystemTask(item, { assistants: [value] }).success, true);
  activities[1].round = 1;
  assert.equal(verifySystemTask(item, { assistants: [value] }).checks.recoveryTrace, false);
});

test('strict JSON and per-indicator denominators keep format, grounding and total success distinct', () => {
  assert.equal(parseFinalObject('```json\n{}\n```'), null);
  assert.equal(parseFinalObject('[]'), null);
  assert.equal(summarizeSystemChecks([{ checks: { completed: true } }, { checks: { completed: false, factFields: true } }]).factFields.total, 1);
});
