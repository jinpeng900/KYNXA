import assert from 'node:assert/strict';
import { link, mkdir, mkdtemp, readFile, readdir, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { AppSkillService } from '../tools/skill-service.mjs';
import { parseSkillFrontmatter, validateSkillFrontmatter } from '../tools/skill-frontmatter.mjs';
import { SKILL_PACKAGE_LIMITS } from '../tools/skill-resources.mjs';
import { within } from '../platform/tool-paths.mjs';

const content = (name, extra = '', body = 'Read resources only when needed.') =>
  `---\nname: ${name}\ndescription: Useful instructions for an explicit task.\n${extra}---\n${body}\n`;
const config = { skillDirectories: [], disabledSkills: [] };
const capabilities = { available: true, sandbox: 'appcontainer', commands: ['node', 'cmd'], network: false,
  skillExecution: { manifestVersion: 1, readOnlyPackage: true, hashChecked: true } };

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-skill-package-')), data = join(root, 'Data');
  const service = new AppSkillService(data, { bundledDirectory: null });
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const save = async (directory, name, files = {}, extra = '') => {
    const path = join(directory, name);
    await mkdir(path, { recursive: true });
    await writeFile(join(path, 'SKILL.md'), content(name, extra));
    for (const [relativePath, value] of Object.entries(files)) {
      const file = join(path, relativePath); await mkdir(join(file, '..'), { recursive: true }); await writeFile(file, value);
    }
    return path;
  };
  const available = async (name, context = null, currentConfig = config) =>
    (await service.list(context, currentConfig, { includeDisabled: true })).find(skill => skill.name === name);
  return { root, data, service, save, available };
}

test('skill header cache skips repeated body reads while revalidating changes, disablement and private paths', async t => {
  const f = await fixture(t), folder = await f.save(join(f.data, 'Skills'), 'cached-skill');
  const first = await f.available('cached-skill'), coldReads = f.service.metadataCacheStats.reads;
  assert.equal(coldReads, 1);
  first.diagnostics.push({ code: 'CALLER_MUTATION', message: 'Must not mutate cached metadata.' });
  const warm = await f.available('cached-skill');
  assert.equal(f.service.metadataCacheStats.reads, coldReads);
  assert.ok(f.service.metadataCacheStats.hits >= 1);
  assert.equal(warm.diagnostics.some(item => item.code === 'CALLER_MUTATION'), false);
  const disabled = { ...config, disabledSkills: [warm.id] };
  assert.deepEqual(await f.service.list(null, disabled), []);
  await assert.rejects(f.service.read(warm.id, null, disabled), { code: 'APP_SKILL_DISABLED' });
  assert.equal(f.service.metadataCacheStats.reads, coldReads);
  const body = await f.service.read(warm.id, null, config);
  assert.equal(f.service.metadataCacheStats.reads, coldReads + 1, 'body reads always re-hash the real file');
  assert.ok(body.content.includes('Read resources'));
  await writeFile(join(folder, 'SKILL.md'), content('cached-skill', '', 'Changed instructions for a fresh task.'));
  const changed = await f.available('cached-skill');
  assert.notEqual(changed.sha256, warm.sha256);
  assert.equal(f.service.metadataCacheStats.reads, coldReads + 2);
  f.service.resourceDeny = path => path === join(folder, 'SKILL.md');
  const denied = await f.available('cached-skill');
  assert.equal(denied.status, 'unavailable');
  assert.equal(denied.diagnostics[0].code, 'PROTECTED_SKILL_RESOURCE');
  f.service.resourceDeny = undefined;
  await rm(join(folder, 'SKILL.md'));
  assert.equal(await f.available('cached-skill'), undefined);
  assert.equal(f.service.metadataCache.size, 0);
});

test('standard validation enforces exact limits, international names, directory match and string metadata without rejecting readable legacy files', () => {
  const name = 'n'.repeat(64);
  const source = `---\nname: ${name}\ndescription: ${'d'.repeat(1024)}\ncompatibility: ${'c'.repeat(500)}\nmetadata: {version: "1.0"}\nallowed-tools: filesystem.read\n---\n`;
  assert.equal(validateSkillFrontmatter(source, { directoryName: name }).valid, true);
  assert.equal(validateSkillFrontmatter(content('数据-分析'), { directoryName: '数据-分析' }).valid, true);
  const legacy = content('Legacy_Mixed', 'compatibility: ' + 'c'.repeat(501) + '\nmetadata: {version: 1, nested: {allowed: true}}\nallowed-tools: [read]\nextra: previous-extension\n');
  assert.ok(parseSkillFrontmatter(legacy));
  const validation = validateSkillFrontmatter(legacy, { directoryName: 'different' });
  assert.equal(validation.valid, false);
  assert.deepEqual(validation.diagnostics.map(item => item.code), ['SKILL_NAME_FORMAT', 'SKILL_DIRECTORY_NAME',
    'SKILL_COMPATIBILITY_LENGTH', 'SKILL_METADATA_FORMAT', 'SKILL_ALLOWED_TOOLS_FORMAT', 'SKILL_UNKNOWN_FIELD']);
  for (const [field, code, replacement] of [['name', 'SKILL_NAME_LENGTH', 'n'.repeat(65)],
    ['description', 'SKILL_DESCRIPTION_LENGTH', 'd'.repeat(1025)], ['compatibility', 'SKILL_COMPATIBILITY_LENGTH', 'c'.repeat(501)]]) {
    assert.ok(validateSkillFrontmatter(source.replace(new RegExp(`^${field}:.*$`, 'm'), `${field}: ${replacement}`)).diagnostics.some(item => item.code === code));
  }
  for (const name of ['-leading', 'trailing-', 'double--dash', 'Uppercase', 'not_name'])
    assert.equal(validateSkillFrontmatter(content(name), { directoryName: name }).valid, false);
  assert.equal(validateSkillFrontmatter(content('valid', 'compatibility: ""\n')).valid, false);
});

test('same source is deduplicated, name conflicts have stable priorities and disabled skills cannot bypass any skill endpoint', async t => {
  const f = await fixture(t), bundled = join(f.root, 'Bundled');
  await f.save(bundled, 'same-name'); await f.save(join(f.data, 'Skills'), 'same-name');
  f.service.bundledDirectory = bundled;
  const currentConfig = { skillDirectories: [join(f.data, 'Skills'), bundled], disabledSkills: [] };
  const all = await f.service.list(null, currentConfig, { includeDisabled: true });
  assert.equal(all.length, 2); assert.deepEqual(all.map(skill => skill.origin), ['builtin', 'data']);
  assert.ok(all.every(skill => skill.conflict.preferredId === all[0].id && skill.diagnostics.some(item => item.code === 'SKILL_NAME_CONFLICT')));
  assert.equal(f.service.discovery.get(all).conflictCount, 1);
  currentConfig.disabledSkills = [all[0].id];
  const enabled = await f.service.list(null, currentConfig);
  assert.equal(enabled.length, 1); assert.equal(enabled[0].id, all[1].id); assert.equal(enabled[0].conflict.preferred, true);
  assert.equal((await f.service.list(null, currentConfig, { includeDisabled: true }))[0].enabled, false);
  for (const call of [() => f.service.read(all[0].id, null, currentConfig),
    () => f.service.readResource(all[0].id, 'SKILL.md', null, currentConfig),
    () => f.service.inspect(all[0].id, null, currentConfig), () => f.service.checkEnvironment(all[0].id, null, currentConfig),
    () => f.service.prepareScript(all[0].id, 'scripts/main.mjs', null, currentConfig)])
    await assert.rejects(call(), { code: 'APP_SKILL_DISABLED' });
  assert.deepEqual((await f.service.list(null, config, { includeDisabled: true })).map(skill => skill.id), all.map(skill => skill.id));
});

test('resources resolve from the owning skill, remain lazy, page without splitting Unicode and retain binary assets as metadata', async t => {
  const f = await fixture(t), text = 'A'.repeat(15999) + '😀中文tail';
  const folder = await f.save(join(f.data, 'Skills'), 'resource-skill', { 'references/guide.md': text, 'assets/picture.png': Buffer.from([137, 80, 78, 71, 0, 255]) });
  const work = join(f.root, 'Work'); await mkdir(join(work, 'references'), { recursive: true });
  await writeFile(join(work, 'references', 'guide.md'), 'Different workspace resource');
  const context = { workspaceRoot: work }, skill = await f.available('resource-skill', context);
  assert.equal(Object.hasOwn(skill, 'content'), false);
  const first = await f.service.readResource(skill.id, 'references/guide.md', context, config);
  assert.equal(first.content, 'A'.repeat(15999) + '😀'); assert.equal(first.nextOffset, 16000); assert.equal(first.hasMore, true);
  const second = await f.service.readResource(skill.id, 'references\\guide.md', context, config, { offset: first.nextOffset });
  assert.equal(second.content, '中文tail'); assert.equal(second.hasMore, false); assert.equal(first.sha256, second.sha256);
  const binary = await f.service.readResource(skill.id, 'assets/picture.png', context, config);
  assert.equal(binary.kind, 'binary'); assert.equal(binary.mimeType, 'image/png'); assert.equal(binary.content, null);
  assert.equal(JSON.stringify(binary).includes('iVBOR'), false);
  const inspection = await f.service.inspect(skill.id, context, config);
  assert.deepEqual(inspection.files.map(file => file.path), ['SKILL.md', 'assets/picture.png', 'references/guide.md']);
  assert.equal(inspection.packageValid, true); assert.equal(await readFile(join(folder, 'references', 'guide.md'), 'utf8'), text);
  await assert.rejects(f.service.readResource(skill.id, 'references/guide.md', context, config, { limit: 16001 }), { code: 'INVALID_TOOL_ARGUMENTS' });
});

test('resource paths reject traversal, absolute paths, Windows devices/streams and credential files without reading their bytes', async t => {
  const f = await fixture(t);
  const folder = await f.save(join(f.data, 'Skills'), 'secure-skill', { 'references/guide.md': 'Allowed', '.env': 'FAKE_PRIVATE_KEY',
    'assets/credentials.json': '{"key":"FAKE_PRIVATE_KEY"}', 'scripts/main.mjs': 'console.log("safe")' });
  const skill = await f.available('secure-skill');
  for (const path of ['../outside.md', 'references/../../outside.md', join(f.root, 'outside.md'), 'C:\\outside.txt',
    '\\\\server\\share\\x', 'references/guide.md:secret', 'references/NUL.txt', 'references/trailing.', '\0bad'])
    await assert.rejects(f.service.readResource(skill.id, path, null, config), error => /SKILL_RESOURCE_PATH/.test(error.code));
  for (const path of ['.env', 'assets/credentials.json'])
    await assert.rejects(f.service.readResource(skill.id, path, null, config), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inventory = await f.service.inspect(skill.id, null, config);
  assert.equal(inventory.packageValid, false); assert.equal(JSON.stringify(inventory).includes('FAKE_PRIVATE_KEY'), false);
  assert.ok(inventory.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
  await assert.rejects(f.service.prepareScript(skill.id, 'scripts/main.mjs', null, config), { code: 'UNSAFE_SKILL_PACKAGE' });
  await assert.rejects(f.service.importPackage(folder), { code: 'INVALID_APP_SKILL_IMPORT' });
});

test('junctions/symlinks and hardlinks cannot expose resources outside a package or enter the script manifest', async t => {
  const f = await fixture(t), external = join(f.root, 'External');
  await mkdir(external); await writeFile(join(external, 'secret.txt'), 'External private bytes');
  const folder = await f.save(join(f.data, 'Skills'), 'linked-skill', { 'scripts/main.mjs': 'console.log("safe")' });
  await mkdir(join(folder, 'references'));
  await symlink(external, join(folder, 'references', 'escape'), process.platform === 'win32' ? 'junction' : 'dir');
  await link(join(external, 'secret.txt'), join(folder, 'references', 'hard.txt'));
  const skill = await f.available('linked-skill');
  for (const path of ['references/escape/secret.txt', 'references/hard.txt'])
    await assert.rejects(f.service.readResource(skill.id, path, null, config), { code: 'UNSAFE_TOOL_PATH' });
  const report = await f.service.inspect(skill.id, null, config);
  assert.equal(report.packageValid, false); assert.equal(JSON.stringify(report).includes('External private bytes'), false);
  assert.ok(report.diagnostics.filter(item => item.code === 'UNSAFE_TOOL_PATH').length >= 2);
  await assert.rejects(f.service.prepareScript(skill.id, 'scripts/main.mjs', null, config), { code: 'UNSAFE_SKILL_PACKAGE' });
});

test('safe script preparation provides a deterministic hashed whitelist without executing code or installing dependencies', async t => {
  const f = await fixture(t), marker = join(f.root, 'should-not-execute');
  const folder = await f.save(join(f.data, 'Skills'), 'node-skill', { 'scripts/main.mjs': 'throw new Error("never execute during inspection")',
    'references/guide.md': 'Read-only resources', 'assets/template.json': '{"example":true}' });
  const skill = await f.available('node-skill'), context = { sandboxCapabilities: capabilities };
  const environment = await f.service.checkEnvironment(skill.id, context, config);
  assert.equal(environment.canRun, true); assert.equal(environment.scripts[0].runtime, 'node');
  const preparation = await f.service.prepareScript(skill.id, 'scripts\\main.mjs', context, config);
  assert.equal(preparation.skillRoot, folder); assert.equal(preparation.scriptRelativePath, 'scripts/main.mjs');
  assert.equal(preparation.environment.canRun, true); assert.equal(preparation.files.length, 4);
  assert.ok(preparation.files.every(file => /^[a-f0-9]{64}$/.test(file.sha256) && file.path.startsWith(folder)));
  assert.equal(preparation.files.reduce((bytes, file) => bytes + file.size, 0), preparation.totalBytes);
  assert.deepEqual((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).files, preparation.files);
  await assert.rejects(readFile(marker), { code: 'ENOENT' });
});

test('environment inspection diagnoses runtimes, manifests and network without spawning a host command', async t => {
  const f = await fixture(t);
  await f.save(join(f.data, 'Skills'), 'dependent-skill', { 'scripts/main.mjs': 'console.log("never run")', 'scripts/extract.py': 'raise Exception("never run")',
    'package.json': JSON.stringify({ dependencies: { 'fake-example': '^1.0.0' }, scripts: { postinstall: 'DO_NOT_EXECUTE' } }),
    'requirements.txt': '# comment\nexample-numpy==1.0\n' }, 'compatibility: Requires Python and access to the internet\n');
  const skill = await f.available('dependent-skill'), context = { sandboxCapabilities: capabilities };
  const report = await f.service.checkEnvironment(skill.id, context, config);
  assert.equal(report.canRun, false); assert.equal(report.compatible, false);
  assert.deepEqual(report.dependencies.map(item => item.ecosystem), ['npm', 'python']);
  assert.ok(report.diagnostics.some(item => item.code === 'SKILL_RUNTIME_UNAVAILABLE'));
  assert.ok(report.diagnostics.some(item => item.code === 'SKILL_NETWORK_UNAVAILABLE'));
  assert.ok(report.diagnostics.some(item => item.code === 'SKILL_DEPENDENCIES_UNVERIFIED'));
  await assert.rejects(f.service.prepareScript(skill.id, 'scripts/extract.py', context, config), { code: 'APP_SKILL_SCRIPT_UNSUPPORTED' });
  assert.equal((await f.service.checkEnvironment(skill.id, null, config)).canRun, false);
});

test('selected Node scripts can run beside unsupported alternatives, but unknown compatibility and unavailable runtime versions do not pass preflight', async t => {
  const f = await fixture(t), major = Number(process.versions.node.split('.')[0]);
  const folder = await f.save(join(f.data, 'Skills'), 'versioned-skill', { 'scripts/main.mjs': 'console.log("safe")',
    'scripts/alternative.py': '# Unsupported alternative', 'package.json': JSON.stringify({ engines: { node: `>=${major}` } }) }, `compatibility: Requires Node.js >=${major}\n`);
  const skill = await f.available('versioned-skill'), context = { sandboxCapabilities: capabilities };
  const preparation = await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config);
  assert.equal(preparation.environment.canRun, true);
  assert.equal(preparation.environment.dependencies[0].status, 'verified');
  assert.ok(preparation.diagnostics.some(item => item.code === 'SKILL_RUNTIME_UNAVAILABLE' && item.severity === 'warning'));
  for (const requirement of [`${major}`, `<=${major}`, `>${major - 1}`]) {
    await writeFile(join(folder, 'package.json'), JSON.stringify({ engines: { node: requirement } }));
    assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, true, requirement);
  }
  await writeFile(join(folder, 'SKILL.md'), content('versioned-skill', 'compatibility: Requires an unspecified third-party product\n'));
  assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, false);
  await writeFile(join(folder, 'SKILL.md'), content('versioned-skill', `compatibility: Requires Node.js >=${major + 1}\n`));
  const unavailable = await f.service.checkEnvironment(skill.id, context, config);
  assert.equal(unavailable.canRun, false);
  assert.ok(unavailable.diagnostics.some(item => item.code === 'SKILL_RUNTIME_VERSION_UNAVAILABLE'));
});

test('descriptive compatibility and documentation URLs do not reject a verified dependency-free Node script', async t => {
  const f = await fixture(t);
  const folder = await f.save(join(f.data, 'Skills'), 'descriptive-skill', { 'scripts/main.mjs': 'console.log("safe")' },
    'compatibility: Designed for coding agents. Documentation at https://example.com/node. No Python is required.\n');
  const skill = await f.available('descriptive-skill'), context = { sandboxCapabilities: capabilities };
  const report = await f.service.checkEnvironment(skill.id, context, config);
  assert.equal(report.compatible, null, 'unverified descriptive metadata is not falsely certified');
  assert.equal(report.canRun, true, 'the selected script has a verified runtime and no missing manifest dependencies');
  assert.ok(report.diagnostics.some(item => item.code === 'SKILL_COMPATIBILITY_REVIEW'));
  assert.ok(!report.diagnostics.some(item => item.code === 'SKILL_NETWORK_UNAVAILABLE'));
  assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, true);
  for (const declaration of ['Node.js', 'Requires Node.js; no network required', `Requires Node.js >=${process.versions.node.split('.')[0]}; without network`]) {
    await writeFile(join(folder, 'SKILL.md'), content('descriptive-skill', `compatibility: ${declaration}\n`));
    assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, true, declaration);
  }
  await writeFile(join(folder, 'package.json'), JSON.stringify({ dependencies: { 'uninstalled-example': '1.0.0' } }));
  assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, false,
    'informational compatibility cannot bypass an unverified actual package dependency');
});

test('an older terminal-only helper cannot pass skill environment checks without explicit read-only hash-checked package capability', async t => {
  const f = await fixture(t);
  await f.save(join(f.data, 'Skills'), 'legacy-helper-skill', { 'scripts/main.mjs': 'console.log("safe")' });
  const skill = await f.available('legacy-helper-skill');
  for (const proof of [undefined, { manifestVersion: 1, readOnlyPackage: true },
    { manifestVersion: 1, readOnlyPackage: false, hashChecked: true },
    { manifestVersion: 2, readOnlyPackage: true, hashChecked: true }]) {
    const context = { sandboxCapabilities: { ...capabilities, skillExecution: proof } };
    const report = await f.service.checkEnvironment(skill.id, context, config);
    assert.equal(report.canRun, false); assert.equal(report.scripts[0].supported, false);
    assert.ok(report.diagnostics.some(item => item.code === 'SKILL_SANDBOX_UNSUPPORTED'));
    assert.equal((await f.service.prepareScript(skill.id, 'scripts/main.mjs', context, config)).environment.canRun, false);
  }
  assert.equal((await f.service.checkEnvironment(skill.id, { sandboxCapabilities: capabilities }, config)).canRun, true);
});

test('skill ownership is rechecked for each resource and a different work cannot reuse a known ID', async t => {
  const f = await fixture(t), work1 = join(f.root, 'Work1'), work2 = join(f.root, 'Work2');
  await f.save(join(work1, '.kynxa', 'skills'), 'work-guidance', { 'references/guide.md': 'Only work1' });
  await mkdir(work2);
  const id = (await f.available('work-guidance', { workspaceRoot: work1 })).id;
  assert.equal((await f.service.readResource(id, 'references/guide.md', { workspaceRoot: work1 }, config)).content, 'Only work1');
  await assert.rejects(f.service.readResource(id, 'references/guide.md', { workspaceRoot: work2 }, config), { code: 'APP_SKILL_NOT_FOUND' });
});

test('oversized packages return diagnostics and cannot become a partially validated script snapshot', async t => {
  const f = await fixture(t), files = Object.fromEntries(Array.from({ length: 128 }, (_, i) => [`assets/${i}.txt`, 'bounded']));
  files['scripts/main.mjs'] = 'console.log("never run")';
  await f.save(join(f.data, 'Skills'), 'many-files', files);
  const skill = await f.available('many-files'), inventory = await f.service.inspect(skill.id, null, config);
  assert.equal(inventory.truncated, true); assert.ok(inventory.files.length <= SKILL_PACKAGE_LIMITS.files);
  assert.ok(inventory.diagnostics.some(item => item.code === 'SKILL_PACKAGE_LIMIT'));
  await assert.rejects(f.service.prepareScript(skill.id, 'scripts/main.mjs', null, config), { code: 'UNSAFE_SKILL_PACKAGE' });
  const big = await f.save(join(f.data, 'Skills'), 'big-resource', { 'assets/big.txt': Buffer.alloc(SKILL_PACKAGE_LIMITS.fileBytes + 1) });
  const oversized = await f.available('big-resource');
  await assert.rejects(f.service.readResource(oversized.id, 'assets/big.txt', null, config), { code: 'SKILL_RESOURCE_LIMIT' });
  await assert.rejects(f.service.importPackage(big), { code: 'INVALID_APP_SKILL_IMPORT' });
});

test('strict import initializes portable Data/Skills and is idempotent without overwriting a different same-name package', async t => {
  const f = await fixture(t), external = join(f.root, 'External');
  const source = await f.save(external, 'portable-skill', { 'scripts/main.mjs': 'console.log("safe")', 'references/guide.md': 'Original guide' });
  const initial = await f.service.importPackage(source);
  assert.equal(initial.imported, true); assert.equal(initial.reused, false);
  assert.equal(initial.skill.origin, 'data'); assert.equal(initial.skill.standardCompliant, true);
  assert.equal((await f.available('portable-skill')).id, initial.skill.id);
  assert.equal(await readFile(join(f.data, 'Skills', 'portable-skill', 'references', 'guide.md'), 'utf8'), 'Original guide');
  const repeated = await f.service.importPackage(source);
  assert.equal(repeated.imported, false); assert.equal(repeated.reused, true); assert.equal(repeated.skill.id, initial.skill.id);
  assert.deepEqual(await readdir(join(f.data, 'Agent', 'skill-imports')), []);
  await writeFile(join(source, 'references', 'guide.md'), 'Different guide');
  await assert.rejects(f.service.importPackage(source), { code: 'APP_SKILL_CONFLICT' });
  assert.equal(await readFile(join(f.data, 'Skills', 'portable-skill', 'references', 'guide.md'), 'utf8'), 'Original guide');
  assert.equal(await readFile(join(source, 'references', 'guide.md'), 'utf8'), 'Different guide');
});

test('concurrent equivalent imports converge on one package and cancellation creates no installation', async t => {
  const f = await fixture(t), source = await f.save(join(f.root, 'External'), 'concurrent-skill', { 'scripts/main.mjs': 'console.log("safe")' });
  const results = await Promise.all([f.service.importPackage(source), f.service.importPackage(source)]);
  assert.equal(results.filter(result => result.imported).length, 1); assert.equal(results.filter(result => result.reused).length, 1);
  assert.equal(results[0].skill.id, results[1].skill.id); assert.equal((await f.service.list(null, config)).length, 1);
  const cancelled = await f.save(join(f.root, 'External'), 'cancelled-skill');
  const controller = new AbortController(); controller.abort();
  await assert.rejects(f.service.importPackage(cancelled, { signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(readFile(join(f.data, 'Skills', 'cancelled-skill', 'SKILL.md')), { code: 'ENOENT' });
});

test('importing an equivalent bundled package reuses its stable ID without copying a duplicate into Data', async t => {
  const f = await fixture(t), bundled = join(f.root, 'Bundled'), files = { 'references/guide.md': 'Already included' };
  await f.save(bundled, 'bundled-guide', files);
  const external = await f.save(join(f.root, 'External'), 'bundled-guide', files);
  f.service.bundledDirectory = bundled;
  const initial = await f.available('bundled-guide'), result = await f.service.importPackage(external);
  assert.equal(result.imported, false); assert.equal(result.reused, true); assert.equal(result.skill.id, initial.id);
  assert.equal(result.skill.origin, 'builtin');
  await assert.rejects(readFile(join(f.data, 'Skills', 'bundled-guide', 'SKILL.md')), { code: 'ENOENT' });
  assert.equal((await f.service.list(null, config)).length, 1);
});

test('a package changing after validation is not committed and the staging directory is cleaned', async t => {
  const f = await fixture(t), source = await f.save(join(f.root, 'External'), 'changed-skill', { 'scripts/main.mjs': 'Original bytes' });
  const originalValidation = f.service.validateImport.bind(f.service);
  f.service.validateImport = async (...args) => {
    const validated = await originalValidation(...args);
    await writeFile(join(source, 'scripts', 'main.mjs'), 'Changed after validation');
    return validated;
  };
  await assert.rejects(f.service.importPackage(source), { code: 'APP_SKILL_CHANGED' });
  await assert.rejects(readFile(join(f.data, 'Skills', 'changed-skill', 'SKILL.md')), { code: 'ENOENT' });
  assert.deepEqual(await readdir(join(f.data, 'Agent', 'skill-imports')), []);
});

test('legacy metadata remains discoverable with diagnostics, but strict import cannot bypass validation', async t => {
  const f = await fixture(t), folder = await f.save(join(f.data, 'Skills'), 'legacy-skill');
  const source = content('Legacy Mixed Name', 'metadata: {nested: {example: true}}\n');
  await writeFile(join(folder, 'SKILL.md'), source);
  const skill = await f.available('Legacy Mixed Name');
  assert.equal(skill.standardCompliant, false); assert.ok(skill.diagnostics.some(item => item.code === 'SKILL_NAME_FORMAT'));
  assert.equal((await f.service.read(skill.id, null, config)).content, source);
  const report = await f.service.validateImport(folder, { strict: false });
  assert.equal(report.valid, false);
  await assert.rejects(f.service.importPackage(folder, { strict: false }), { code: 'INVALID_APP_SKILL_IMPORT' });
  assert.equal(await readFile(join(folder, 'SKILL.md'), 'utf8'), source);
});

test('a configured skill above Data cannot read or snapshot formal config or private raw tool results', async t => {
  const f = await fixture(t), name = basename(f.root);
  await writeFile(join(f.root, 'SKILL.md'), content(name));
  await mkdir(join(f.root, 'scripts')); await writeFile(join(f.root, 'scripts', 'main.mjs'), 'console.log("safe")');
  const secretConfig = join(f.data, 'Agent', 'config.json'), secretResult = join(f.data, 'Chats', 'session-id', 'tool-results', 'raw_result.json');
  for (const file of [secretConfig, secretResult]) {
    await mkdir(join(file, '..'), { recursive: true }); await writeFile(file, 'FAKE_PRIVATE_PAYLOAD');
  }
  // On Windows TEMP may use an 8.3 username alias; discovery deliberately uses its canonical long path.
  // Windows 的 TEMP 路径可能使用 8.3 用户名别名；发现流程有意使用规范长路径。
  const canonicalParent = await realpath(f.root);
  const current = { skillDirectories: [canonicalParent], disabledSkills: [] }, skill = await f.available(name, null, current);
  assert.ok(skill && skill.status !== 'unavailable');
  for (const path of ['Data/Agent/config.json', 'Data/Chats/session-id/tool-results/raw_result.json'])
    await assert.rejects(f.service.readResource(skill.id, path, null, current), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inventory = await f.service.inspect(skill.id, null, current);
  assert.equal(inventory.packageValid, false);
  assert.ok(inventory.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE'));
  assert.equal(JSON.stringify(inventory).includes('FAKE_PRIVATE_PAYLOAD'), false);
  assert.equal(inventory.files.some(file => file.path.startsWith('Data/')), false);
  const environment = await f.service.checkEnvironment(skill.id, { sandboxCapabilities: capabilities }, current);
  assert.equal(environment.canRun, false);
  await assert.rejects(f.service.prepareScript(skill.id, 'scripts/main.mjs', null, current), { code: 'UNSAFE_SKILL_PACKAGE' });
  await assert.rejects(f.service.importPackage(f.root), { code: 'INVALID_APP_SKILL_IMPORT' });
  await assert.rejects(f.service.importPackage(canonicalParent), { code: 'INVALID_APP_SKILL_IMPORT' });
  assert.equal(await readFile(secretConfig, 'utf8'), 'FAKE_PRIVATE_PAYLOAD');
  assert.equal(await readFile(secretResult, 'utf8'), 'FAKE_PRIVATE_PAYLOAD');
});

test('custom privacy callbacks apply throughout resources and manifests while ordinary external config.json stays readable', async t => {
  const f = await fixture(t);
  const folder = await f.save(join(f.root, 'External'), 'custom-skill', { 'scripts/main.mjs': 'console.log("safe")',
    'config.json': '{"publicSetting":true}', 'private/config.json': 'FAKE_CONFIG_SECRET', 'artifacts/raw_result.json': 'FAKE_RAW_SECRET' });
  const denied = [join(folder, 'private'), join(folder, 'artifacts', 'raw_result.json')], checked = [];
  const service = new AppSkillService(f.data, { bundledDirectory: null, denyResource: path => {
    checked.push(path); return denied.some(root => within(root, path));
  } });
  const current = { skillDirectories: [join(f.root, 'External')], disabledSkills: [] };
  const skill = (await service.list(null, current)).find(item => item.name === 'custom-skill');
  assert.equal((await service.readResource(skill.id, 'config.json', null, current)).content, '{"publicSetting":true}');
  for (const path of ['private/config.json', 'artifacts/raw_result.json'])
    await assert.rejects(service.readResource(skill.id, path, null, current), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inventory = await service.inspect(skill.id, null, current);
  assert.equal(inventory.packageValid, false);
  assert.equal(JSON.stringify(inventory).includes('FAKE_CONFIG_SECRET'), false);
  assert.equal(JSON.stringify(inventory).includes('FAKE_RAW_SECRET'), false);
  await assert.rejects(service.prepareScript(skill.id, 'scripts/main.mjs', null, current), { code: 'UNSAFE_SKILL_PACKAGE' });
  await assert.rejects(service.importPackage(folder), { code: 'INVALID_APP_SKILL_IMPORT' });
  assert.ok(checked.some(path => path === join(folder, 'config.json')));
});

test('formal Data allows skills and canonical GUID work folders, but not an arbitrary project-named data directory', async t => {
  const f = await fixture(t), projectId = '0123456789abcdef0123456789abcdef';
  await f.save(join(f.data, 'Skills'), 'data-guidance', { 'references/config.json': '{"public":"skill"}' });
  const workspaceRoot = join(f.data, 'Desktop', 'Projects', projectId);
  await f.save(join(workspaceRoot, '.kynxa', 'skills'), 'work-guidance', { 'references/config.json': '{"public":"work"}' });
  const context = { workspaceRoot }, skills = await f.service.list(context, config);
  for (const name of ['data-guidance', 'work-guidance']) {
    const skill = skills.find(item => item.name === name); assert.ok(skill && skill.status !== 'unavailable');
    assert.equal((await f.service.readResource(skill.id, 'references/config.json', context, config)).kind, 'text');
  }
  const invalidWork = join(f.data, 'Desktop', 'Projects', 'not-a-project-id');
  const folder = await f.save(join(invalidWork, '.kynxa', 'skills'), 'invalid-work');
  const invalid = await f.service.list({ workspaceRoot: invalidWork }, config, { includeDisabled: true });
  assert.ok(invalid.some(skill => skill.status === 'unavailable' && skill.diagnostics.some(item => item.code === 'PROTECTED_SKILL_RESOURCE')));
  await assert.rejects(f.service.importPackage(folder), { code: 'PROTECTED_SKILL_RESOURCE' });
});

test('a lexical privacy callback also covers canonical Windows aliases inside otherwise public Data/Skills', async t => {
  const f = await fixture(t);
  const folder = await f.save(join(f.data, 'Skills'), 'callback-skill', { 'references/config.json': '{"public":true}',
    'private/raw_result.json': 'FAKE_PRIVATE_RAW_RESULT' });
  const denied = join(folder, 'private');
  const service = new AppSkillService(f.data, { bundledDirectory: null, denyResource: path => within(denied, path) });
  const current = { skillDirectories: [await realpath(join(f.data, 'Skills'))], disabledSkills: [] };
  const discovered = await service.list(null, current);
  const skill = discovered.find(item => item.name === 'callback-skill');
  assert.equal(discovered.filter(item => item.name === 'callback-skill').length, 1);
  assert.equal((await service.readResource(skill.id, 'references/config.json', null, current)).kind, 'text');
  await assert.rejects(service.readResource(skill.id, 'private/raw_result.json', null, current), { code: 'PROTECTED_SKILL_RESOURCE' });
  const inspection = await service.inspect(skill.id, null, current);
  assert.equal(inspection.packageValid, false); assert.equal(JSON.stringify(inspection).includes('FAKE_PRIVATE_RAW_RESULT'), false);
  await assert.rejects(service.importPackage(await realpath(folder)), { code: 'INVALID_APP_SKILL_IMPORT' });
  current.disabledSkills = [skill.id];
  assert.equal((await service.list(null, current)).some(item => item.name === 'callback-skill'), false);
});
