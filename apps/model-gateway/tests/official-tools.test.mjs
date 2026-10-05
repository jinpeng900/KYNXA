import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdir, mkdtemp, readFile, readdir, rm, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { AppSkillService } from '../tools/skill-service.mjs';
import { curatedMcpPresets as compatibilityPresets } from '../tools/mcp-preset-catalog.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { curatedMcpPresets, OFFICIAL_SKILLS_DIRECTORY, OFFICIAL_TOOLS_PACKAGE_ID, OFFICIAL_TOOLS_ROOT,
  normalizeOfficialDisabledSkills, officialSkillIdentity, readOfficialToolsManifest } from '../tools/official-tools.mjs';
import { toolFixture } from './tool-fixture.mjs';

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const pathId = path => hash(resolve(path)).slice(0, 24);
const config = { skillDirectories: [], disabledSkills: [] };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-official-tools-')), data = join(root, 'Data');
  await mkdir(data);
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  return { root, data, service: new AppSkillService(data) };
}

test('official inventory locates the single packaged tool, MCP and skill sources without mutating shared metadata', async () => {
  const manifest = await readOfficialToolsManifest();
  assert.equal(manifest.packageId, OFFICIAL_TOOLS_PACKAGE_ID);
  assert.match(manifest.version, /^\d+\.\d+\.\d+$/);
  assert.equal(manifest.license, 'Apache-2.0');
  assert.strictEqual(compatibilityPresets, curatedMcpPresets);
  assert.equal(manifest.skills.length, 7);
  assert.equal(manifest.mcpPresets.length, 11);
  assert.equal(manifest.coreTools.length, builtinDescriptors.length);
  assert.deepEqual(manifest.coreTools, builtinDescriptors.map(tool => tool.name));
  assert.equal(new Set(manifest.coreTools).size, manifest.coreTools.length);
  assert.deepEqual(manifest.mcpPresets.map(preset => preset.id), curatedMcpPresets.map(preset => preset.id));
  assert.ok(builtinDescriptors.every(tool => tool.source === 'builtin' && tool.inputSchema.type === 'object'));
  for (const preset of manifest.mcpPresets) {
    const catalog = curatedMcpPresets.find(item => item.id === preset.id);
    assert.equal(preset.sourceUrl, catalog.sourceUrl);
    assert.equal(preset.license, catalog.license);
    assert.deepEqual(preset.package, catalog.package);
    assert.equal(preset.distribution, catalog.package ? 'external-runtime' : 'hosted-service');
  }
  manifest.skills.length = 0;
  manifest.coreTools[0] = 'fixture.mutated';
  const untouched = await readOfficialToolsManifest();
  assert.equal(untouched.skills.length, 7);
  assert.equal(untouched.coreTools[0], 'filesystem.list');
});

test('all migrated skill resources retain recorded bytes, origin notices and usable lazy resources', async t => {
  const f = await fixture(t), manifest = await readOfficialToolsManifest();
  assert.equal(f.service.bundledDirectory, resolve(OFFICIAL_SKILLS_DIRECTORY));
  const listed = await f.service.list(null, config);
  assert.deepEqual(listed.map(skill => skill.name).sort(), manifest.skills.map(skill => skill.name).sort());
  assert.ok(listed.every(skill => skill.origin === 'builtin' && skill.standardCompliant && !Object.hasOwn(skill, 'content')));
  for (const skill of manifest.skills) {
    const folder = join(OFFICIAL_SKILLS_DIRECTORY, dirname(skill.path)), actual = [];
    async function inventory(directory = '') {
      for (const entry of await readdir(join(folder, directory), { withFileTypes: true })) {
        const path = directory ? `${directory}/${entry.name}` : entry.name;
        if (entry.isDirectory()) await inventory(path);
        else actual.push(path);
      }
    }
    await inventory();
    assert.deepEqual(actual.sort(), skill.files.map(file => file.path).sort());
    for (const file of skill.files) assert.equal(hash(await readFile(join(folder, file.path))), file.sha256);
    assert.equal(listed.find(item => item.name === skill.name).id, officialSkillIdentity(skill.path).id);
    const loaded = await f.service.read(officialSkillIdentity(skill.path).id, null, config);
    assert.ok(loaded.content.includes(skill.name));
    for (const sourceFile of [skill.source.licenseFile, skill.source.provenanceFile].filter(Boolean)) {
      const sourcePath = resolve(OFFICIAL_TOOLS_ROOT, sourceFile), suffix = relative(OFFICIAL_TOOLS_ROOT, sourcePath);
      assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
      assert.ok((await stat(sourcePath)).isFile());
    }
  }
  const comms = listed.find(skill => skill.name === 'internal-comms');
  const example = await f.service.readResource(comms.id, 'examples/3p-updates.md', null, config);
  assert.ok(example.content.length > 100);
  assert.match(await readFile(join(OFFICIAL_SKILLS_DIRECTORY, 'internal-comms', 'UPSTREAM.md'), 'utf8'), /8a1541c4a3ffa5a20a5a91de0dcf3f0bab1d1ef4/);
  assert.match(await readFile(join(OFFICIAL_SKILLS_DIRECTORY, 'internal-comms', 'LICENSE.txt'), 'utf8'), /Apache License/);
  await assert.rejects(stat(join(dirname(OFFICIAL_TOOLS_ROOT.replace(/[\\/]$/, '')), 'skills')), { code: 'ENOENT' });
});

test('official skill identity survives installation moves while custom packages retain their path identities', async t => {
  const f = await fixture(t), name = 'workspace-inspect', path = `${name}/SKILL.md`;
  const first = officialSkillIdentity(path, { skillsDirectory: join(f.root, 'Installation-A', 'official-tools', 'Skills'),
    legacySkillsDirectory: join(f.root, 'Installation-A', 'skills') });
  const second = officialSkillIdentity(path, { skillsDirectory: join(f.root, 'Installation-B', 'official-tools', 'Skills'),
    legacySkillsDirectory: join(f.root, 'Installation-B', 'skills') });
  assert.equal(first.id, second.id);
  assert.notDeepEqual(first.legacyIds, second.legacyIds);
  assert.equal(first.id, officialSkillIdentity(`${name}\\SKILL.md`).id);
  for (const invalid of ['../workspace-inspect/SKILL.md', '/workspace-inspect/SKILL.md', 'C:\\fixture\\SKILL.md',
    'workspace-inspect/./SKILL.md', 'workspace-inspect//SKILL.md', 'workspace-inspect/SKILL.md:stream', 'workspace-inspect/not-a-skill.md'])
    assert.throws(() => officialSkillIdentity(invalid), TypeError);
  const customRoot = join(f.root, 'Custom');
  await cp(join(OFFICIAL_SKILLS_DIRECTORY, name), join(customRoot, name), { recursive: true });
  const customService = new AppSkillService(f.data, { bundledDirectory: customRoot });
  const custom = (await customService.list(null, config))[0];
  assert.equal(custom.id, pathId(join(customRoot, name, 'SKILL.md')));
  assert.notEqual(custom.id, first.id);
  const withCustom = await f.service.list(null, { ...config, skillDirectories: [customRoot] });
  assert.equal(withCustom.filter(skill => skill.name === name).length, 2);
  assert.ok(withCustom.find(skill => skill.id === first.id).conflict.preferred);
});

test('old disabled IDs still block every official skill endpoint and normalize for re-enabling without touching custom settings', async t => {
  const f = await fixture(t), { id, legacyIds } = officialSkillIdentity('internal-comms/SKILL.md');
  for (const disabledId of [id, ...legacyIds]) {
    const disabledConfig = { ...config, disabledSkills: [disabledId] };
    assert.equal((await f.service.list(null, disabledConfig)).some(skill => skill.id === id), false);
    assert.equal((await f.service.list(null, disabledConfig, { includeDisabled: true })).find(skill => skill.id === id).enabled, false);
    for (const call of [() => f.service.read(id, null, disabledConfig),
      () => f.service.readResource(id, 'SKILL.md', null, disabledConfig), () => f.service.inspect(id, null, disabledConfig),
      () => f.service.checkEnvironment(id, null, disabledConfig), () => f.service.prepareScript(id, 'scripts/fixture.mjs', null, disabledConfig)])
      await assert.rejects(call(), { code: 'APP_SKILL_DISABLED' });
  }
  const custom = pathId(join(f.root, 'Custom', 'internal-comms', 'SKILL.md'));
  assert.deepEqual(await normalizeOfficialDisabledSkills([legacyIds[0], id, legacyIds[1], custom]), [id, custom]);
  assert.deepEqual(await normalizeOfficialDisabledSkills(undefined), []);
  assert.equal((await f.service.list(null, { ...config, disabledSkills: [custom] })).find(skill => skill.id === id).enabled, true);
});

test('importing the unchanged official skill reuses its installed package instead of creating a user duplicate', async t => {
  const f = await fixture(t), directory = join(OFFICIAL_SKILLS_DIRECTORY, 'internal-comms');
  const result = await f.service.importPackage(directory);
  assert.equal(result.imported, false);
  assert.equal(result.reused, true);
  assert.equal(result.skill.origin, 'builtin');
  assert.equal(result.skill.id, officialSkillIdentity('internal-comms/SKILL.md').id);
  await assert.rejects(stat(join(f.data, 'Skills', 'internal-comms')), { code: 'ENOENT' });
});

test('the real default broker lists the installed package and rejects changes to its manifest, catalogs and skills', async t => {
  const f = await toolFixture(t, { officialTools: true }), context = await f.context('full');
  const settings = await f.service.getConfig();
  assert.equal(settings.officialPackageVersion, (await readOfficialToolsManifest()).version);
  assert.equal(settings.mcpServers.filter(server => curatedMcpPresets.some(preset => preset.id === server.presetId || preset.id === server.id)).length, 11);
  assert.equal((await f.service.listSkills(context)).filter(skill => skill.origin === 'builtin').length, 7);
  for (const path of ['manifest.json', 'MCP/catalog.mjs', 'Tools/catalog.mjs', 'Skills/workspace-inspect/SKILL.md']) {
    const file = join(OFFICIAL_TOOLS_ROOT, path), original = await readFile(file, 'utf8');
    const result = await f.run(context, 'filesystem.write', { path: file, content: original,
      expectedHash: hash(Buffer.from(original)), reason: 'Synthetic package immutability verification.' }, { interactive: false });
    assert.equal(result.isError, true);
    assert.equal(result.code, 'PROTECTED_APP_DATA');
    assert.equal(await readFile(file, 'utf8'), original);
  }
});

test('desktop MSBuild selects every official package asset once using the published relative layout', { skip: process.platform !== 'win32' }, async () => {
  const repository = fileURLToPath(new URL('../../../', import.meta.url));
  const evaluated = spawnSync('dotnet', ['msbuild', 'apps/desktop/KYNXA.Desktop.csproj', '-p:Platform=x64',
    '-p:Configuration=Release', '-getItem:Content', '-nologo'], { cwd: repository, encoding: 'utf8', windowsHide: true, maxBuffer: 4 * 1024 * 1024 });
  assert.equal(evaluated.status, 0, evaluated.error?.message ?? evaluated.stderr);
  const content = JSON.parse(evaluated.stdout).Items.Content;
  const official = content.filter(item => item.Link?.replaceAll('\\', '/').startsWith('model-gateway/official-tools/'));
  const manifest = await readOfficialToolsManifest();
  const expected = ['manifest.json', 'LICENSE.txt', 'MCP/catalog.mjs', 'Tools/catalog.mjs', 'Tools/computer.mjs', 'Tools/terminal.mjs',
    ...manifest.skills.flatMap(skill => skill.files.map(file => `Skills/${dirname(skill.path).replaceAll('\\', '/')}/${file.path}`))];
  assert.deepEqual(official.map(item => item.Link.replaceAll('\\', '/').slice('model-gateway/official-tools/'.length)).sort(), expected.sort());
  assert.ok(official.every(item => item.CopyToOutputDirectory === 'PreserveNewest' && item.CopyToPublishDirectory === 'PreserveNewest'));
  assert.equal(content.some(item => item.Link?.replaceAll('\\', '/').startsWith('model-gateway/skills/')), false);
  assert.equal(content.some(item => /official-tools.*\.(?:pdb|tmp)$/i.test(item.Link ?? '')), false);
});
