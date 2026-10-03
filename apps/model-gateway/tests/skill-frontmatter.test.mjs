import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve, sep } from 'node:path';
import { test } from 'node:test';
import { parseSkillFrontmatter } from '../skill-frontmatter.mjs';
import { AppSkillService } from '../skill-service.mjs';

const skill = header => `---\n${header}\n---\n# Literal original body\nDo not execute automatically.\n`;
const basic = 'name: standard-skill\ndescription: Standard description';

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'kynxa-yaml-skills-'));
  t.after(async () => {
    const suffix = relative(resolve(tmpdir()), resolve(root));
    assert.ok(suffix && suffix !== '..' && !suffix.startsWith(`..${sep}`));
    await rm(root, { recursive: true, force: true });
  });
  const service = new AppSkillService(root, { bundledDirectory: null }), config = { skillDirectories: [] };
  const save = async (directory, source) => {
    const folder = join(root, 'Skills', directory); await mkdir(folder, { recursive: true });
    const file = join(folder, 'SKILL.md'); await writeFile(file, source); return file;
  };
  return { root, service, config, save };
}

test('standard quote, escaped scalar, comments and folded/literal descriptions preserve their intended text', () => {
  const cases = [
    ['name: "quoted-name" # trailing comment\ndescription: "Quoted # description" # another comment', 'Quoted # description'],
    ["name: 'quoted-name' # comment\ndescription: 'It''s a quoted description' # comment", "It's a quoted description"],
    ['name: quoted-name\ndescription: "Line\\nTab\\tUnicode\\u4f60\\u597d\\nLiteral quote \\\" and slash \\\\"', 'Line\nTab\tUnicode你好\nLiteral quote " and slash \\'],
    ['name: quoted-name\ndescription: plain # trailing comment', 'plain'],
    ['name: quoted-name\ndescription: >\n  Fold the first line\n  with the second line', 'Fold the first line with the second line'],
    ['name: quoted-name\ndescription: |-\n  Keep the first line\n  and the second line', 'Keep the first line\nand the second line'],
    ['name: quoted-name\ndescription: >-\n  Fold one\n  and two\n\n  New paragraph', 'Fold one and two\nNew paragraph']
  ];
  for (const [header, expected] of cases) {
    const parsed = parseSkillFrontmatter(skill(header));
    assert.equal(parsed?.name, 'quoted-name', header); assert.equal(parsed.description, expected, header);
  }
  assert.deepEqual(parseSkillFrontmatter('\uFEFF' + skill(basic).replace(/\n/g, '\r\n')),
    { name: 'standard-skill', description: 'Standard description' });
});

test('optional standard metadata and core scalar types are data, preserving previous name and description limits', () => {
  const parsed = parseSkillFrontmatter(skill(`${basic}\nlicense: MIT\ncompatibility: "Node >=22"\nallowed-tools: [filesystem.read, filesystem.stat]\nmetadata:\n  author: sample\n  version: 1.2\n  nested:\n    enabled: true\n    nullable: null\n    platforms: [windows, linux]\n  release: 2026-01-01`));
  assert.deepEqual(parsed.metadata, { license: 'MIT', compatibility: 'Node >=22', 'allowed-tools': ['filesystem.read', 'filesystem.stat'],
    metadata: { author: 'sample', version: 1.2, nested: { enabled: true, nullable: null, platforms: ['windows', 'linux'] }, release: '2026-01-01' } });
  assert.equal(parseSkillFrontmatter(skill(`${basic}\nallowed-tools: filesystem.read filesystem.stat`)).metadata['allowed-tools'], 'filesystem.read filesystem.stat');
  assert.equal(parseSkillFrontmatter(skill(`name: ${'n'.repeat(100)}\ndescription: ${'d'.repeat(2000)}`)).name.length, 100);
  assert.equal(parseSkillFrontmatter(skill(`name: ${'n'.repeat(101)}\ndescription: valid`)), null);
  assert.equal(parseSkillFrontmatter(skill(`name: valid\ndescription: ${'d'.repeat(2001)}`)), null);
  assert.equal(parseSkillFrontmatter(skill('name: "  Legacy Mixed Name  "\ndescription: "  Legacy description  "')).name, 'Legacy Mixed Name');
});

test('duplicate keys, aliases, unsafe tags, merge/prototype keys and malformed metadata are rejected', () => {
  const cases = [
    `${basic}\nname: repeated`, `${basic}\nmetadata: {same: 1, same: 2}`, `${basic}\nmetadata: {anchor: &a value, reused: *a}`,
    'name: &n allowed-name\ndescription: *n', `${basic}\nmetadata: !custom value`, `${basic}\nmetadata: !!js/function function() {}`,
    `${basic}\nmetadata: {base: &b {a: 1}, copied: {<<: *b}}`, `${basic}\nmetadata: {'<<': {a: 1}}`,
    `${basic}\nmetadata: {__proto__: {polluted: true}}`, `${basic}\nmetadata: {constructor: value}`, `${basic}\nmetadata: {prototype: value}`,
    `${basic}\nmetadata: {1: numeric-key}`, `${basic}\nmetadata: []`, `${basic}\nlicense: [MIT]`, `${basic}\ncompatibility: true`,
    `${basic}\nallowed-tools: [filesystem.read, 42]`, `${basic}\nmetadata: {number: .inf}`, `${basic}\nmetadata: {number: .nan}`,
    `${basic}\nmetadata: {broken`, 'name: true\ndescription: valid', 'name: valid\ndescription: null'
  ];
  for (const header of cases) assert.equal(parseSkillFrontmatter(skill(header)), null, header);
  assert.equal({}.polluted, undefined);
  for (const value of [null, undefined, 42, {}, [], 'missing header', '---\nname: missing-description\n---'])
    assert.equal(parseSkillFrontmatter(value), null);
});

test('header byte, AST node and nesting limits stop excessive metadata before conversion', () => {
  assert.ok(parseSkillFrontmatter(skill(`${basic}\n# ${'x'.repeat(32000)}`)));
  assert.equal(parseSkillFrontmatter(skill(`${basic}\n# ${'x'.repeat(32768)}`)), null);
  assert.equal(parseSkillFrontmatter(skill(`${basic}\n# ${'汉'.repeat(11000)}`)), null, 'header limit counts UTF-8 bytes');
  assert.ok(parseSkillFrontmatter(skill(`${basic}\nmetadata:\n${Array.from({ length: 450 }, (_, i) => `  item-${i}: value`).join('\n')}`)));
  assert.equal(parseSkillFrontmatter(skill(`${basic}\nmetadata:\n${Array.from({ length: 520 }, (_, i) => `  item-${i}: value`).join('\n')}`)), null);
  const nested = depth => { const metadata = {}; let node = metadata; for (let i = 0; i < depth; i++) { node.next = {}; node = node.next; } return JSON.stringify(metadata); };
  assert.ok(parseSkillFrontmatter(skill(`${basic}\nmetadata: ${nested(5)}`)));
  assert.equal(parseSkillFrontmatter(skill(`${basic}\nmetadata: ${nested(13)}`)), null);
});

test('real application discovery and lazy read accept quoted/commented and multiline YAML without executing guidance', async t => {
  const f = await fixture(t);
  const cases = [
    ['quotes', 'name: quoted-skill\ndescription: "Quoted # text" # user comment\nmetadata: {author: sample}', 'Quoted # text'],
    ['folded', 'name: folded-skill\ndescription: >\n  A useful folded\n  description', 'A useful folded description'],
    ['literal', "name: literal-skill\ndescription: |-\n  One line\n  Another line\nallowed-tools: 'terminal.run'", 'One line\nAnother line']
  ];
  for (const [directory, header] of cases) await f.save(directory, skill(header));
  const available = await f.service.list(null, f.config);
  assert.equal(available.length, cases.length); assert.equal(f.service.discovery.get(available).unavailableCount, 0);
  for (const [directory, header, description] of cases) {
    const found = available.find(item => item.source.includes(`${sep}${directory}${sep}`));
    assert.equal(found.description, description); assert.equal(Object.hasOwn(found, 'content'), false);
    const loaded = await f.service.read(found.id, null, f.config);
    assert.equal(loaded.content, skill(header)); assert.equal(await readFile(found.source, 'utf8'), loaded.content);
  }
  assert.equal(available.find(item => item.name === 'literal-skill').metadata['allowed-tools'], 'terminal.run');
  assert.deepEqual((await f.service.list(null, f.config)).map(item => item.id), available.map(item => item.id));
});

test('invalid UTF-8, binary, oversized files and restricted YAML are isolated while originals and valid skills remain intact', async t => {
  const f = await fixture(t), originals = new Map();
  for (const [directory, bytes] of [
    ['invalid-utf8', Buffer.concat([Buffer.from(skill(basic)), Buffer.from([0xc3, 0x28])])],
    ['binary', Buffer.from(skill(basic) + '\0')],
    ['oversized-file', Buffer.from(skill(basic) + 'x'.repeat(256 * 1024))],
    ['oversized-header', Buffer.from(skill(`${basic}\n# ${'x'.repeat(32768)}`))],
    ['alias', Buffer.from(skill('name: &name unsafe-skill\ndescription: *name'))],
    ['custom-tag', Buffer.from(skill(`${basic}\nmetadata: !custom value`))],
    ['duplicate', Buffer.from(skill(`${basic}\nname: repeated`))],
    ['deep', Buffer.from(skill(`${basic}\nmetadata: ${'{next:'.repeat(15)}value${'}'.repeat(15)}`))]
  ]) originals.set(await f.save(directory, bytes), bytes);
  await f.save('healthy', skill(basic));
  const list = await f.service.list(null, f.config);
  assert.equal(list.length, originals.size + 1); assert.equal(list.filter(item => item.status === 'unavailable').length, originals.size);
  assert.equal(f.service.discovery.get(list).unavailableCount, originals.size);
  const healthy = list.find(item => item.name === 'standard-skill'); assert.ok(healthy); assert.equal(healthy.status, undefined);
  assert.equal((await f.service.read(healthy.id, null, f.config)).content, skill(basic));
  for (const [file, original] of originals) {
    const unavailable = list.find(item => item.source === file); assert.equal(unavailable.status, 'unavailable');
    await assert.rejects(f.service.read(unavailable.id, null, f.config), { code: 'INVALID_APP_SKILL' });
    assert.deepEqual(await readFile(file), original);
  }
});
