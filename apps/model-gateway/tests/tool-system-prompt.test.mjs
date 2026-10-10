import assert from 'node:assert/strict';
import { test } from 'node:test';
import { estimateTokens } from '../models/context-tokens.mjs';
import { buildToolSystemPrompt } from '../tools/tool-system-prompt.mjs';

const context = { permissionMode: 'ask', workspaceRoot: '<synthetic-work-folder>',
  desktopCapabilities: { available: true }, sandboxCapabilities: { available: false },
  hostTerminalCapabilities: { available: true, backgroundJobs: true } };

test('prompt separates semantic tool choice, explicit target limits and schema readiness from execution authority', () => {
  const prompt = buildToolSystemPrompt(context, { maximumTokens: 1300, catalogState: {
    selectionState: 'configured', availableCount: 8, selectedCount: 3, deferredCount: 5,
    tools: [{ name: 'computer.type', state: 'available', schema: 'deferred' }],
    servers: [{ name: 'mcp.synthetic', state: 'ready' }] } });
  assert.match(prompt, /meaning of the original request/);
  assert.match(prompt, /Keywords\/task projections only rank candidates/);
  assert.match(prompt, /Invoke exact declared function names \(k_\*\); guidance uses logical names\. tool\.load takes logical names\./);
  assert.match(prompt, /Exclusions and target\/window\/local\/remote limits bind every action/);
  assert.match(prompt, /Empty tool\.search pages all permitted tools\/services/);
  assert.match(prompt, /zero matches do not mean inability/);
  assert.match(prompt, /"name":"computer.type","state":"available","schema":"deferred"/);
  assert.match(prompt, /"authentication":"unknown"/);
  assert.match(prompt, /user handles trust\/MFA/);
  assert.ok(estimateTokens(prompt) <= 1300, estimateTokens(prompt));
});

test('runtime fact projection reports unavailable and authentication states without copying credentials or metadata', () => {
  const prompt = buildToolSystemPrompt(context, { catalogState: {
    selectionState: 'pending', availableCount: 4,
    tools: [{ name: 'terminal.host.run', state: 'unavailable', schema: 'selection-pending', code: 'HOST_TERMINAL_UNAVAILABLE',
      description: 'PRIVATE_DESCRIPTION', token: 'PRIVATE_TOKEN', executionEnvironment: {
        executorLocation: 'gateway-host', operationLocation: 'gateway-host', userDeviceRelationship: 'unverified',
        connectionUrl: 'PRIVATE_ENDPOINT' } }],
    servers: [{ name: 'mcp.synthetic', state: 'auth-required', authentication: 'required', code: 'MCP_AUTH_REQUIRED',
      args: ['PRIVATE_ARGUMENT'], token: 'PRIVATE_TOKEN' }] } });
  assert.match(prompt, /"selection":"pending","availableCount":4/);
  assert.match(prompt, /HOST_TERMINAL_UNAVAILABLE/);
  assert.match(prompt, /"authentication":"required"/);
  assert.match(prompt, /"environment":"gateway-host\/gateway-host"/);
  assert.doesNotMatch(prompt, /PRIVATE_|"selectedCount"|"deferredCount"/);
});

test('optional plan and memory guidance names only actually available entries and keeps semantic proposals revisable', () => {
  const absent = buildToolSystemPrompt(context);
  assert.doesNotMatch(absent, /knowledge\.plan:|memory\.read:|memory\.propose:/);
  const prompt = buildToolSystemPrompt(context, { maximumTokens: 1600,
    catalogState: { availableNames: ['knowledge.plan', 'memory.read', 'memory.propose'] } });
  assert.match(prompt, /knowledge\.plan: optional scoped candidates/);
  assert.match(prompt, /proposals need evidence, never truth\/authority/);
  assert.match(prompt, /memory\.read: authorized originals\/revisions/);
  assert.match(prompt, /memory\.propose: cited drafts for user confirmation/);
  assert.match(prompt, /interpretations revisable/);
  assert.ok(estimateTokens(prompt) <= 1600, estimateTokens(prompt));
});

test('skill header deferral preserves the compact permission and effect contracts within a small prompt budget', () => {
  const prompt = buildToolSystemPrompt(context, { maximumTokens: 1200,
    skills: Array.from({ length: 12 }, (_, index) => ({ id: `skill-${index}`, name: 'Synthetic skill',
      description: 'Optional metadata '.repeat(20), content: 'PRIVATE_SKILL_BODY' })) });
  assert.ok(estimateTokens(prompt) <= 1200, estimateTokens(prompt));
  assert.match(prompt, /effects, env\/credentials/);
  assert.match(prompt, /Observe interrupted\/unknown effects before retry/);
  assert.match(prompt, /omitted headers remain discoverable/);
  assert.doesNotMatch(prompt, /PRIVATE_SKILL_BODY/);
});
