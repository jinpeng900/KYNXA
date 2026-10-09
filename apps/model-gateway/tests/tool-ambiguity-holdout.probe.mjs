import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { analyzeRequestClauses } from '../platform/request-clause-signals.mjs';
import { toolSelectionSignals } from '../tools/tool-discovery.mjs';
import { ModelToolCatalog } from '../tools/tool-catalog.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { inferBrowserTaskIntent, canUseBrowserServer, assertBrowserLaunchAllowed } from '../tools/browser-intent-policy.mjs';

const manifestUrl = new URL('./fixtures/tool-ambiguity-holdout-20261009.json', import.meta.url);
const manifest = JSON.parse(await readFile(manifestUrl, 'utf8'));
if (!manifest.frozenBeforeRun) throw new Error('Expected behavior must be frozen before probing.');
const resultPath = process.argv[2] ?? join(tmpdir(), 'kynxa-tool-ambiguity-holdout-latest.json');
const chrome = { id: 'probe-existing-chrome', name: 'Probe existing Chrome', command: 'npx', enabled: true,
  args: ['chrome-devtools-mcp@1.10.1', '--autoConnect'] };
const edge = { ...chrome, id: 'probe-existing-edge', args: [...chrome.args, '--executablePath=C:\\Probe\\msedge.exe'] };
const launches = intent => Object.fromEntries(['chrome', 'msedge', 'firefox'].map(name => {
  try { assertBrowserLaunchAllowed({ browserTaskIntent: intent }, 'C:\\Probe\\' + name + '.exe'); return [name, { allowed: true }]; }
  catch (error) { return [name, { allowed: false, code: error.code }]; }
}));
const sourceModules = ['../platform/request-clause-signals.mjs', '../tools/tool-discovery.mjs',
  '../tools/tool-catalog.mjs', '../tools/browser-intent-policy.mjs'];
const moduleDigests = Object.fromEntries(await Promise.all(sourceModules.map(async modulePath => [
  modulePath, createHash('sha256').update(await readFile(new URL(modulePath, import.meta.url))).digest('hex')
])));

// Frozen semantic expectations precede implementation observations; all operations below are pure/read-only.
// 语义预期先冻结再观察实现；以下只调用纯函数或只读策略，不启动浏览器、不派发工具、不额外调用模型。
const results = manifest.cases.map(item => {
  const history = item.history ?? [];
  const signals = toolSelectionSignals(item.message, { historySignals: history,
    previousToolNames: history.length ? ['computer.launch', 'mcp.chrome.navigate_page'] : [] });
  const intent = inferBrowserTaskIntent(item.message, history);
  const catalog = new ModelToolCatalog(builtinDescriptors, { protocol: 'openai-completions',
    tokenBudget: 16000, message: item.message, historySignals: history,
    previousToolNames: history.length ? ['computer.launch', 'mcp.chrome.navigate_page'] : [] });
  const actual = { intent, existingChromeAllowed: canUseBrowserServer({ browserTaskIntent: intent }, chrome),
    existingEdgeAllowed: canUseBrowserServer({ browserTaskIntent: intent }, edge), launches: launches(intent),
    signals: { browser: signals.browser, desktop: signals.desktop, remoteBrowser: signals.remoteBrowser,
      hostTerminal: signals.hostTerminal, deviceState: signals.deviceState, docs: signals.docs, web: signals.web,
      taskRelation: signals.taskRelation, retainedNames: [...signals.retainedNames] },
    clauseProjection: analyzeRequestClauses(item.message),
    visibleSchemaNames: catalog.selected.map(tool => tool.name),
    discoveryEscapeHatch: ['tool.search', 'tool.load'].every(name => catalog.selected.some(tool => tool.name === name)) };
  const mismatches = Object.entries(item.expected).filter(([key, expected]) =>
    (key === 'allowLocalBrowser' || key === 'explicitBrowserTask' ? intent[key] : actual[key]) !== expected)
    .map(([field, expected]) => ({ field, expected,
      actual: field === 'allowLocalBrowser' || field === 'explicitBrowserTask' ? intent[field] : actual[field] }));
  const classification = !mismatches.length ? 'matches-frozen-expectation'
    : mismatches.some(mismatch => mismatch.field === 'allowLocalBrowser' && mismatch.actual === true)
      ? 'execution-boundary-false-positive'
      : mismatches.some(mismatch => mismatch.field === 'existingChromeAllowed' && mismatch.actual === true)
        ? 'app-restriction-not-enforced'
        : 'execution-boundary-false-negative';
  const impact = classification === 'execution-boundary-false-positive'
    ? 'A quoted, rejected, hypothetical or unresolved-condition operation may pass the local-browser intent gate if the main model calls it. This does not show an actual operation or bypass other broker checks.'
    : classification === 'app-restriction-not-enforced'
      ? 'The current app-wide Chrome restriction does not filter the Chrome connection; the main model must still interpret the original restriction.'
      : classification === 'execution-boundary-false-negative'
        ? 'A current affirmative operation may be blocked by the browser intent gate; search/load cannot grant authorization.'
        : actual.signals.browser || actual.signals.desktop || actual.signals.hostTerminal
          ? 'Capability hints are advisory candidates. A candidate hit alone is not a tool-selection error or authorization.'
          : 'The boundary agrees with the frozen semantic expectation; no model selection or real execution was measured.';
  return { ...item, actual, mismatches, classification, impact,
    responsibility: mismatches.length ? 'programmatic-gate-mismatch-with-semantic-expectation'
      : 'main-model-interprets-original-user-text-and-selects-an-action' };
});
const counts = results.reduce((summary, result) => {
  summary[result.classification] = (summary[result.classification] ?? 0) + 1; return summary;
}, {});
const byCategory = Object.fromEntries([...new Set(results.map(item => item.category))].map(category => {
  const entries = results.filter(item => item.category === category);
  return [category, { total: entries.length, matches: entries.filter(item => !item.mismatches.length).length,
    mismatches: entries.filter(item => item.mismatches.length).map(item => item.id) }];
}));
const report = { schemaVersion: 1, generatedAt: new Date().toISOString(), manifest: fileURLToPath(manifestUrl),
  manifestSha256: createHash('sha256').update(await readFile(manifestUrl)).digest('hex'), moduleDigests,
  limitations: manifest.disclaimer, checks: 'Pure production APIs, launch guards and connection filters; no actual browser/tool dispatch or model calls.',
  total: results.length, counts, byCategory, results };
await writeFile(resultPath, JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({ total: results.length, counts, byCategory, resultPath }, null, 2));
for (const item of results.filter(item => item.mismatches.length))
  console.log(JSON.stringify({ id: item.id, message: item.message, expected: item.expected,
    actualIntent: item.actual.intent, existingChromeAllowed: item.actual.existingChromeAllowed,
    classification: item.classification, mismatches: item.mismatches }));
