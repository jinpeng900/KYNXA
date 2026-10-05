import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { test } from 'node:test';
import { modelHome } from '../data/storage.mjs';

test('storage pointer relocates model data; explicit environment paths have priority', async () => {
  const userHome = await mkdtemp(join(tmpdir(), 'kynxa-paths-'));
  assert.equal(modelHome({}, userHome), join(userHome, '.kynxa', 'models'));
  await mkdir(join(userHome, '.kynxa'));
  const root = join(userHome, 'relocated');
  await writeFile(join(userHome, '.kynxa', 'storage.json'), JSON.stringify({ dataRoot: root }));
  assert.equal(modelHome({}, userHome), join(root, 'Models'));
  const override = join(userHome, 'override');
  assert.equal(modelHome({ KYNXA_DATA_HOME: override }, userHome), join(override, 'Models'));
  assert.equal(modelHome({ KYNXA_MODEL_HOME: override }, userHome), override);
  await writeFile(join(userHome, '.kynxa', 'storage.json'), JSON.stringify({ dataRoot: 'relative' }));
  assert.throws(() => modelHome({}, userHome), /绝对路径/);
});
