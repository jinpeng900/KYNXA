import { basename } from 'node:path';
import { createHash } from 'node:crypto';

export const SYSTEM_SUITE_VERSION = 'agent-system-v2';
export const SYSTEM_VERIFIER_VERSION = 'structured-facts-and-effects-v1';
export const SYSTEM_LIMITS = Object.freeze({ maxRounds: 12, maxToolCalls: 24,
  maxGeneratedTokens: 98304, maxDurationMs: 120000 });
const json = fields => `只返回一个 JSON 对象，不要代码围栏或过程说明。字段：${fields}。sources 是实际依据的文件名数组；未知值为 null，不能猜测。`;

const record = '# Synthetic launch record\n\n'
  + 'Project IRIS has an experimental window on 2034-05-18 at 16:25 UTC. '
  + 'Its safety reviewer is Nora Vale. This window is not an approved launch.\n\n'
  + 'Project LARCH has an experimental window on 2035-07-21 at 08:10 UTC. '
  + 'Its safety reviewer is Emil Reed.\n';
const chinese = '# 合成部署记录\n\n海棠的试运行窗口为2036年3月9日11:20 UTC。'
  + '审核人是林澈。目前仅为试运行，正式发布尚未获准。\n';
const old = '# Synthetic KESTREL schedule\n\nRevision: 1. Recorded: 2037-02-10. '
  + 'KESTREL planned window: 2037-04-02 10:00 UTC; reviewer Ada North.\n';
const latest = '# Synthetic KESTREL schedule\n\nRevision: 2. Recorded: 2037-03-11. '
  + 'This replaces revision 1. KESTREL planned window: 2037-04-08 14:35 UTC; reviewer Bina West.\n';
const approvals = '# Synthetic approvals\n\nIRIS approval: pending. Required condition: redundant valve check. '
  + 'Approval owner: Tomas Gray. The safety reviewer and approval owner have different roles.\n';
const unknown = '# Synthetic PEBBLE record\n\nPEBBLE has no recorded release date or time. Approval: pending. '
  + 'Do not use another project schedule as the PEBBLE schedule.\n';
const long = '# Synthetic equipment handbook\n\n## General\n'
  + Array.from({ length: 150 }, (_, index) => `Maintenance entry ${index + 1}: keep the enclosure clean; this is not a calibration procedure.\n`).join('')
  + '\n## Sensor C19 calibration exception\n\nSensor C19 must wait 37 minutes after the amber indicator clears. '
  + 'Skip calibration when ambient temperature exceeds 41 C. Owner: Priya Moss.\n\n## Packaging\n'
  + 'Packaging requirements do not override calibration exceptions.\n';
const seed = 'ticket=KYNXA-V2-862451\n';
const configuration = '{\n  "mode": "safe",\n  "attempts": 2,\n  "tag": "keep-me"\n}\n';

/** Freeze independent fixtures and gold before inference; only task files enter model authority.
 * 推理前冻结独立资料与标准答案；模型权限只覆盖资料文件，不包含本模块或评分器。 */
export const SYSTEM_TASKS = Object.freeze([
  { id: 'greeting-zh', category: 'routing', messages: ['你好！'], files: {}, greeting: 'zh' },
  { id: 'greeting-en', category: 'routing', messages: ['Hello!'], files: {}, greeting: 'en' },
  { id: 'english-to-chinese', category: 'grounding', files: { 'launch.md': record }, messages: [
    '根据本地 launch.md，给出 IRIS 的实验窗口、UTC时间、安全审查人、是否批准正式发射。'
      + json('entity, date（YYYY-MM-DD）, time（HH:MM UTC）, reviewer, approved（boolean）, sources')],
    gold: { entity: 'IRIS', date: '2034-05-18', time: '16:25 UTC', reviewer: 'Nora Vale', approved: false }, sources: ['launch.md'] },
  { id: 'chinese-to-english', category: 'grounding', files: { '部署.md': chinese }, messages: [
    'Use the local file 部署.md. Return one JSON object only: entity (keep the Chinese project name), '
      + 'date (YYYY-MM-DD), time (HH:MM UTC), reviewer (keep original name), approved (boolean), sources (actual filenames). '
      + 'Report the trial window and whether production release is approved. Use null for unknown fields.'],
    gold: { entity: '海棠', date: '2036-03-09', time: '11:20 UTC', reviewer: '林澈', approved: false }, sources: ['部署.md'] },
  { id: 'latest-revision', category: 'grounding', files: { 'schedule-r1.md': old, 'schedule-r2.md': latest }, messages: [
    '根据本地 schedule-r1.md 与 schedule-r2.md，给出 KESTREL 当前有效计划。采用已生效的最新修订，不能混用旧值。'
      + json('entity, revision（integer）, date（YYYY-MM-DD）, time（HH:MM UTC）, reviewer, sources')],
    gold: { entity: 'KESTREL', revision: 2, date: '2037-04-08', time: '14:35 UTC', reviewer: 'Bina West' }, sources: ['schedule-r2.md'] },
  { id: 'insufficient-evidence', category: 'grounding', files: { 'pebble.md': unknown, 'launch.md': record }, messages: [
    '根据本地资料 pebble.md，PEBBLE 的正式发布时间已确认了吗？给出已知日期与UTC时间；不确定就用null。'
      + json('entity, date, time, approved（boolean）, sources')],
    gold: { entity: 'PEBBLE', date: null, time: null, approved: false }, sources: ['pebble.md'] },
  { id: 'multi-source-join', category: 'grounding', files: { 'launch.md': record, 'approval.md': approvals }, messages: [
    '根据本地 launch.md 和 approval.md，给出 IRIS 的安全审查人、批准负责人、剩余批准条件和批准状态，注意两个角色不同。'
      + json('entity, reviewer, approvalOwner, requiredCondition（保留英文原文）, approved（boolean）, sources')],
    gold: { entity: 'IRIS', reviewer: 'Nora Vale', approvalOwner: 'Tomas Gray', requiredCondition: 'redundant valve check', approved: false },
    sources: ['launch.md', 'approval.md'] },
  { id: 'long-section', category: 'grounding', files: { 'equipment.md': long }, messages: [
    '根据本地 equipment.md，Sensor C19 在琥珀色指示灯清除后要等待几分钟？环境温度超过几摄氏度就跳过校准？负责人是谁？必要时回读相应章节。'
      + json('sensor, waitMinutes（integer）, maximumTemperatureC（integer）, owner, sources')],
    gold: { sensor: 'C19', waitMinutes: 37, maximumTemperatureC: 41, owner: 'Priya Moss' }, sources: ['equipment.md'] },
  { id: 'followup-entity', category: 'history', files: { 'launch.md': record }, messages: [
    '根据本地 launch.md，比较 IRIS 和 LARCH 的实验窗口及审查人。只返回 JSON：projects（entity/date/time/reviewer 数组）、sources。',
    '继续，前一个项目 IRIS 的安全审查人和实验UTC时间是什么？'
      + json('entity, reviewer, time（HH:MM UTC）, sources')],
    firstGold: [
      { entity: 'IRIS', date: '2034-05-18', time: '16:25 UTC', reviewer: 'Nora Vale' },
      { entity: 'LARCH', date: '2035-07-21', time: '08:10 UTC', reviewer: 'Emil Reed' }],
    gold: { entity: 'IRIS', reviewer: 'Nora Vale', time: '16:25 UTC' }, sources: ['launch.md'] },
  { id: 'dependent-file-cycle', category: 'effects', files: { 'seed.txt': seed }, index: false, messages: [
    '先读取工作文件 seed.txt，把内容逐字复制到一个尚不存在的 receipt.txt，再实际重新读取 receipt.txt 核对，不要预测读回。'
      + json('verified（boolean）, ticket（种子内容中等号后的值，不含换行）, sources')],
    gold: { verified: true, ticket: 'KYNXA-V2-862451' }, sources: ['seed.txt', 'receipt.txt'],
    effect: { kind: 'copy', source: 'seed.txt', target: 'receipt.txt', content: seed } },
  { id: 'conflict-aware-edit', category: 'effects', files: { 'config.json': configuration }, index: false, messages: [
    '读取 config.json，把 attempts 改为5，其他字段保持原值。写回必须使用实际读取版本的 expectedHash，之后实际读回确认。'
      + json('verified（boolean）, mode, attempts（integer）, tag, sources')],
    gold: { verified: true, mode: 'safe', attempts: 5, tag: 'keep-me' }, sources: ['config.json'],
    effect: { kind: 'edit', source: 'config.json', target: 'config.json', expected: { mode: 'safe', attempts: 5, tag: 'keep-me' },
      originalHash: createHash('sha256').update(configuration).digest('hex') } },
  { id: 'recover-missing-file', category: 'recovery', files: { 'fallback.md': record }, index: false, messages: [
    '先尝试读取 missing.md，如果不存在，改读 fallback.md，再回答 IRIS 的安全审查人。不要因第一次读取失败就结束整个问答。'
      + json('entity, reviewer, recovered（boolean）, sources')],
    gold: { entity: 'IRIS', reviewer: 'Nora Vale', recovered: true }, sources: ['fallback.md'], recovery: true }
]);

const SUPPORT_SPANS = Object.freeze({
  'english-to-chinese': { 'launch.md': ['IRIS has an experimental window on 2034-05-18 at 16:25 UTC', 'Nora Vale', 'not an approved launch'] },
  'chinese-to-english': { '部署.md': ['2036年3月9日11:20 UTC', '林澈', '正式发布尚未获准'] },
  'latest-revision': { 'schedule-r2.md': ['Revision: 2', 'replaces revision 1', '2037-04-08 14:35 UTC', 'Bina West'] },
  'insufficient-evidence': { 'pebble.md': ['PEBBLE has no recorded release date or time', 'Approval: pending'] },
  'multi-source-join': { 'launch.md': ['IRIS has an experimental window', 'Nora Vale'],
    'approval.md': ['IRIS approval: pending', 'redundant valve check', 'Tomas Gray'] },
  'long-section': { 'equipment.md': ['Sensor C19 must wait 37 minutes', 'temperature exceeds 41 C', 'Priya Moss'] },
  'followup-entity': { 'launch.md': ['IRIS has an experimental window on 2034-05-18 at 16:25 UTC', 'Nora Vale'] },
  'dependent-file-cycle': { 'seed.txt': ['KYNXA-V2-862451'], 'receipt.txt': ['KYNXA-V2-862451'] },
  'conflict-aware-edit': { 'config.json': ['keep-me', 'safe'] },
  'recover-missing-file': { 'fallback.md': ['IRIS has an experimental window', 'Nora Vale'] }
});
export const SYSTEM_SUPPORT_FINGERPRINT_INPUT = SUPPORT_SPANS;

export function parseFinalObject(content) {
  try { const result = JSON.parse(String(content ?? '').trim());
    return result && typeof result === 'object' && !Array.isArray(result) ? result : null;
  } catch { return null; }
}
const parseResult = activity => { try { return JSON.parse(activity.result); } catch { return {}; } };
const observations = assistants => assistants.flatMap(assistant => [
  ...(assistant.BenchmarkObservations ?? []),
  ...(assistant.ToolActivities ?? []).filter(item => item.status === 'completed').flatMap(activity => {
    const result = parseResult(activity);
    if (activity.name === 'filesystem.read') return [{ title: basename(activity.arguments?.path ?? ''), text: result.content ?? '' }];
    return activity.name === 'knowledge.read' ? [{ title: result.title, text: result.text ?? result.content ?? '' }]
      : activity.name === 'knowledge.search' ? (result.items ?? []).map(item => ({ title: item.title, text: item.excerpt ?? '' })) : [];
  })
]);
const observedTitles = assistants => new Set(observations(assistants).map(item => item.title).filter(Boolean));
const equalFields = (value, gold) => value !== null && Object.entries(gold).every(([field, expected]) => value?.[field] === expected);
const exactFields = (value, gold) => equalFields(value, gold) && Object.keys(value).length === Object.keys(gold).length;
const validSources = (value, required, assistants) => {
  const observed = observedTitles(assistants);
  return Array.isArray(value?.sources) && required.every(title => value.sources.includes(title)) &&
    value.sources.length > 0 && value.sources.every(title => typeof title === 'string' && observed.has(title));
};

/** Structured assertions keep entity/role attribution explicit and effects independently observable.
 * 结构化断言显式绑定实体与角色，实际副作用另用磁盘与执行回执核验，不使用模型裁判或事后改分。 */
export function verifySystemTask(task, { assistants, disk = {} }) {
  const checks = { completed: assistants.length === task.messages.length && assistants.every(item => item.Status === 'completed') };
  const last = assistants.at(-1), activities = assistants.flatMap(item => item.ToolActivities ?? []);
  const answer = String(last?.Content ?? ''), parsed = parseFinalObject(answer);
  if (task.greeting) {
    checks.shortGreeting = answer.trim().length > 0 && answer.length <= 120 &&
      (task.greeting === 'zh' ? /你好|您好|嗨|哈喽|早上好|上午好|晚上好/u.test(answer) : /\b(?:hello|hi|hey)\b|\bgood\s+(?:morning|day|evening)\b/iu.test(answer));
    checks.noCapabilityList = !/搜索|浏览器|工具|终端|文件操作|capabilities|file operations|terminal|browser|tools/iu.test(answer);
    checks.noTools = activities.length === 0;
    checks.noEvidence = !assistants.some(item => item.EvidenceReferences?.length);
  } else {
    checks.validFinalJson = parsed !== null;
    checks.factFields = equalFields(parsed, task.gold);
    checks.outputSchema = parsed !== null && Object.keys(parsed).length === Object.keys(task.gold).length + 1;
    checks.citedSources = validSources(parsed, task.sources, assistants);
    const visible = observations(assistants);
    checks.supportedFacts = Object.entries(SUPPORT_SPANS[task.id]).every(([title, spans]) =>
      spans.every(span => visible.some(item => item.title === title && item.text.includes(span))));
    if (task.firstGold) {
      const first = parseFinalObject(assistants[0]?.Content);
      checks.firstTurnFacts = Array.isArray(first?.projects) && first.projects.length === task.firstGold.length &&
        task.firstGold.every(gold => first.projects.some(value => exactFields(value, gold)));
      checks.firstTurnSources = validSources(first, task.sources, assistants.slice(0, 1));
      checks.firstTurnSchema = first !== null && Object.keys(first).length === 2;
      checks.firstTurnSupport = ['Nora Vale', 'Emil Reed', '2034-05-18 at 16:25 UTC', '2035-07-21 at 08:10 UTC'].every(span =>
        observations(assistants.slice(0, 1)).some(item => item.title === 'launch.md' && item.text.includes(span)));
    }
    if (task.effect) {
      const { source, target } = task.effect;
      const readIndex = activities.findIndex(item => item.name === 'filesystem.read' && item.status === 'completed' && basename(item.arguments?.path ?? '') === source);
      const writeIndex = activities.findIndex((item, index) => index > readIndex && item.name === 'filesystem.write' && item.status === 'completed' && basename(item.arguments?.path ?? '') === target);
      const rereadIndex = activities.findIndex((item, index) => index > writeIndex && item.name === 'filesystem.read' && item.status === 'completed' && !item.reused && basename(item.arguments?.path ?? '') === target);
      checks.dependentToolRounds = readIndex >= 0 && writeIndex > readIndex && rereadIndex > writeIndex &&
        activities[readIndex].round < activities[writeIndex].round && activities[writeIndex].round < activities[rereadIndex].round;
      const readBack = rereadIndex >= 0 ? parseResult(activities[rereadIndex]).content : undefined;
      if (task.effect.kind === 'copy') {
        checks.diskState = disk[target] === task.effect.content;
        checks.readBackState = readBack === task.effect.content;
      } else {
        checks.diskState = exactFields(parseFinalObject(disk[target]), task.effect.expected);
        checks.readBackState = exactFields(parseFinalObject(readBack), task.effect.expected);
        checks.conflictHash = writeIndex >= 0 && activities[writeIndex].arguments?.expectedHash === task.effect.originalHash;
      }
    }
    if (task.recovery) {
      const failed = activities.findIndex(item => item.name === 'filesystem.read' && basename(item.arguments?.path ?? '') === 'missing.md' && item.status === 'error');
      checks.recoveryTrace = failed >= 0 && activities.some((item, index) => index > failed && item.name === 'filesystem.read' &&
        basename(item.arguments?.path ?? '') === 'fallback.md' && item.status === 'completed' && item.round > activities[failed].round);
    }
  }
  return { success: Object.values(checks).every(Boolean), checks };
}

export function summarizeSystemChecks(runs) {
  const rate = field => { const applicable = runs.filter(run => Object.hasOwn(run.checks, field));
    return { passed: applicable.filter(run => run.checks[field]).length, total: applicable.length,
      rate: applicable.length ? applicable.filter(run => run.checks[field]).length / applicable.length : null }; };
  return Object.fromEntries([...new Set(runs.flatMap(run => Object.keys(run.checks)))].map(field => [field, rate(field)]));
}
