// Run with the desktop and gateway stopped. Keeps source data as a backup.
import { mkdir, readFile, readdir, writeFile, rename, lstat, copyFile, unlink } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import { createReadStream, constants } from 'node:fs';
import { join, resolve, relative, dirname, basename, isAbsolute, sep, parse } from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { modelHome } from './storage.mjs';
import { inspectDataLayout } from './data-layout.mjs';
import { initializeStorage } from './initialize-storage.mjs';

const conversationNames = [
  'Projects', 'Chats', 'Trash', 'Backups', 'Memory', 'Index',
  'settings.json', 'catalog.json', '.conversations-v1.json', '.catalog-transaction.json'
];
const conversationEntries = new Set(conversationNames.map(name => name.toLowerCase()));
const within = (path, root) => {
  const child = relative(root, path);
  return !child || (child !== '..' && !child.startsWith(`..${sep}`) && !isAbsolute(child));
};
async function optionalRead(path) {
  try { return await readFile(path); } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}
async function rejectLinks(path) {
  for (let current = resolve(path); ; current = dirname(current)) {
    try { if ((await lstat(current)).isSymbolicLink()) throw new Error('Migration paths must not contain symbolic links or junctions.'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (dirname(current) === current) break;
  }
}
async function snapshot(root) {
  await rejectLinks(root.source);
  const files = [], directories = [];
  async function visit(prefix = '') {
    let entries;
    try { entries = await readdir(join(root.source, prefix), { withFileTypes: true }); }
    catch (error) { if (!prefix && error.code === 'ENOENT') return; throw error; }
    for (const entry of entries) {
      if (!prefix && root.include && !root.include.has(entry.name.toLowerCase())) continue;
      if (!prefix && root.exclude?.has(entry.name.toLowerCase())) continue;
      const path = join(prefix, entry.name);
      if (entry.isSymbolicLink()) throw new Error('Linked files require a separate migration: ' + path);
      if (entry.isDirectory()) { directories.push(path); await visit(path); }
      else if (entry.isFile()) files.push(path);
      else throw new Error('Unsupported file type: ' + path);
    }
  }
  await visit();
  return { files: files.sort(), directories: directories.sort() };
}
async function hash(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest('hex');
}
const json = buffer => JSON.parse(buffer.toString('utf8').replace(/^\uFEFF/, ''));

// Explicit arguments make migration testable without touching the current user's
// pointer. CLI callers still use the configured model home and profile pointer.
export async function migrateStorage({ desktopSource, modelSource, target, pointer, progress = () => {} }) {
  if (![desktopSource, modelSource, target, pointer].every(path => typeof path === 'string' && isAbsolute(path)))
    throw new Error('Migration paths must be absolute.');
  desktopSource = resolve(desktopSource); modelSource = resolve(modelSource); target = resolve(target);
  if (target === parse(target).root || target.startsWith('\\\\')) throw new Error('Choose an empty directory on a local disk.');
  const standard = basename(modelSource).toLowerCase() === 'models';
  const conversationRoot = standard ? dirname(modelSource) : join(modelSource, 'Conversations');
  for (const source of [desktopSource, modelSource, ...conversationNames.map(name => join(conversationRoot, name))])
    if (within(target, source) || within(source, target)) throw new Error('Destination must not overlap source directories.');
  await rejectLinks(target);
  try { if ((await readdir(target)).length) throw new Error('Destination must be empty.'); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  await inspectDataLayout(conversationRoot);
  const roots = [
    { source: desktopSource, name: 'Desktop' },
    { source: modelSource, name: 'Models', exclude: standard ? undefined : new Set(['conversations']) },
    { source: conversationRoot, name: '', include: conversationEntries }
  ];
  const snapshots = await Promise.all(roots.map(snapshot));
  const oldPointer = await optionalRead(pointer), verified = [];
  await mkdir(target, { recursive: true });
  for (let index = 0; index < roots.length; index++) {
    const root = roots[index], state = snapshots[index];
    await mkdir(join(target, root.name), { recursive: true });
    for (const directory of state.directories) await mkdir(join(target, root.name, directory), { recursive: true });
    for (const path of state.files) {
      const source = join(root.source, path), destination = join(target, root.name, path);
      await rejectLinks(source);
      const digest = await hash(source);
      await copyFile(source, destination, constants.COPYFILE_EXCL);
      if (digest !== await hash(destination)) throw new Error('Copy verification failed: ' + path);
      verified.push({ source, digest });
    }
  }
  progress('verify');
  for (let index = 0; index < roots.length; index++)
    if (JSON.stringify(snapshots[index]) !== JSON.stringify(await snapshot(roots[index])))
      throw new Error('Source directories changed during migration.');
  for (const file of verified)
    if (file.digest !== await hash(file.source)) throw new Error('Source files changed during migration.');

  let relocatedFolders = 0;
  function relocateFolders(value) {
    if (!value || typeof value !== 'object') return;
    for (const [key, child] of Object.entries(value)) {
      if (key.toLowerCase() === 'folderpath' && typeof child === 'string' && isAbsolute(child)
          && within(child, join(desktopSource, 'Projects'))) {
        value[key] = join(target, 'Desktop', relative(desktopSource, child)); relocatedFolders++;
      } else if (typeof child === 'object') relocateFolders(child);
    }
  }
  for (const path of [join(target, 'Desktop', 'projects.json'), join(target, 'catalog.json'), join(target, '.catalog-transaction.json')]) {
    const source = await optionalRead(path);
    if (!source) continue;
    const value = json(source);
    relocateFolders(value);
    await writeFile(path, JSON.stringify(value, null, 2));
  }
  // project.json is a derived view of catalog metadata. Relocate its managed
  // workspace path now; normal gateway initialization will reconcile the view.
  let projectDirectories = [];
  try { projectDirectories = await readdir(join(target, 'Projects'), { withFileTypes: true }); }
  catch (error) { if (error.code !== 'ENOENT') throw error; }
  for (const directory of projectDirectories) {
    if (!directory.isDirectory()) continue;
    const path = join(target, 'Projects', directory.name, 'project.json'), source = await optionalRead(path);
    if (!source) continue;
    const value = json(source);
    relocateFolders(value);
    await writeFile(path, JSON.stringify(value, null, 2));
  }
  const localServerPath = join(target, 'Models', 'local-server.json');
  const localSource = await optionalRead(localServerPath);
  if (localSource) {
    const value = json(localSource);
    for (const key of ['modelPath', 'serverPath', 'backendPath']) {
      const path = value[key];
      if (typeof path !== 'string' || !isAbsolute(path)) continue;
      for (const root of roots.filter(root => root.name))
        if (within(path, root.source)) { value[key] = join(target, root.name, relative(root.source, path)); break; }
    }
    await writeFile(localServerPath, JSON.stringify(value, null, 2));
  }
  await inspectDataLayout(target);
  await initializeStorage(target);
  await inspectDataLayout(target);
  if (!(oldPointer ?? Buffer.alloc(0)).equals(await optionalRead(pointer) ?? Buffer.alloc(0)))
    throw new Error('Storage pointer changed during migration.');
  if (oldPointer) await writeFile(join(target, 'storage-pointer.previous.json'), oldPointer, { flag: 'wx' });
  await writeFile(join(target, 'migration-info.json'), JSON.stringify({
    version: 1, completedAt: new Date().toISOString(), verifiedFiles: verified.length, originalFilesRetained: true
  }));
  await mkdir(dirname(pointer), { recursive: true });
  const temporary = pointer + '.' + randomUUID() + '.tmp';
  try {
    await writeFile(temporary, JSON.stringify({ version: 1, dataRoot: target }, null, 2), { mode: 0o600, flag: 'wx' });
    await rename(temporary, pointer);
  } finally { await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
  return { dataRoot: target, verifiedFiles: verified.length, relocatedFolders, originalFilesRetained: true };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [desktopSource, target] = process.argv.slice(2);
  if (!desktopSource || !target) throw new Error('Usage: node migrate-storage.mjs ABSOLUTE_DESKTOP_SOURCE ABSOLUTE_NEW_DATA_ROOT');
  console.log(JSON.stringify(await migrateStorage({
    desktopSource, modelSource: modelHome(), target, pointer: join(homedir(), '.kynxa', 'storage.json')
  })));
}
