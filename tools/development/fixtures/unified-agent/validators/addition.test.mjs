import assert from 'node:assert/strict';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { test } from 'node:test';

// This independent file lives outside the agent-visible fixture copy.
// 此独立验收文件位于模型可见样本副本以外。
const { add } = await import(pathToFileURL(join(process.env.KYNXA_EVALUATION_WORKSPACE, 'addition.mjs')).href);
test('addition contract', () => {
  assert.equal(add(2, 3), 5);
  assert.equal(add(-4, 2), -2);
  assert.equal(add(0, 5), 5);
});
