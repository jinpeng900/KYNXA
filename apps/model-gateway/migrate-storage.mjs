// Run with the desktop and gateway stopped. Keeps source data as a backup.
import { cp, mkdir, readFile, readdir, writeFile, rename, access } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { join, resolve, relative, dirname, isAbsolute, sep } from 'node:path';
import { homedir } from 'node:os';
import { modelHome } from './storage.mjs';

const [desktopArg, targetArg] = process.argv.slice(2);
if (!desktopArg || !targetArg || !isAbsolute(desktopArg) || !isAbsolute(targetArg))
  throw new Error('Usage: node migrate-storage.mjs ABSOLUTE_DESKTOP_SOURCE ABSOLUTE_NEW_DATA_ROOT');
const desktopSource = resolve(desktopArg), modelSource = modelHome(), target = resolve(targetArg);
const pointer = join(homedir(), '.kynxa', 'storage.json');
for (const source of [desktopSource, modelSource]) {
  await access(source);
  const relation = relative(source, target);
  if (!relation || (!relation.startsWith(`..${sep}`) && relation !== '..' && !isAbsolute(relation)))
    throw new Error('Destination must not be inside a source directory.');
}
await mkdir(dirname(target), { recursive: true });
// Refuse to merge into a directory which may contain another user's data.
await mkdir(target);

async function files(root, prefix = '') {
  const result = [];
  for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) {
    const path = join(prefix, entry.name);
    if (entry.isSymbolicLink()) throw new Error('Linked files require a separate migration: ' + path);
    if (entry.isDirectory()) result.push(...await files(root, path));
    else if (entry.isFile()) result.push(path);
  }
  return result;
}

async function hash(path) {
  const digest = createHash('sha256');
  for await (const chunk of createReadStream(path)) digest.update(chunk);
  return digest.digest('hex');
}

let verifiedFiles = 0;
for (const [source, name] of [[desktopSource, 'Desktop'], [modelSource, 'Models']]) {
  const paths = await files(source);
  await cp(source, join(target, name), { recursive: true, force: false, errorOnExist: true, preserveTimestamps: true });
  for (const path of paths) {
    if (await hash(join(source, path)) !== await hash(join(target, name, path)))
      throw new Error('Copy verification failed: ' + path);
    verifiedFiles++;
  }
}

const projectPath = join(target, 'Desktop', 'projects.json');
let relocatedFolders = 0;
try {
  const projects = JSON.parse((await readFile(projectPath, 'utf8')).replace(/^\uFEFF/, ''));
  const managed = join(desktopSource, 'Projects');
  for (const project of projects) {
    if (!project.FolderPath) continue;
    const child = relative(managed, project.FolderPath);
    if (!child || (child !== '..' && !child.startsWith(`..${sep}`) && !isAbsolute(child))) {
      project.FolderPath = join(target, 'Desktop', 'Projects', child);
      relocatedFolders++;
    }
  }
  await writeFile(projectPath, JSON.stringify(projects, null, 2));
  const reread = JSON.parse(await readFile(projectPath, 'utf8'));
  if (JSON.stringify(projects) !== JSON.stringify(reread)) throw new Error('Project path verification failed.');
} catch (error) { if (error.code !== 'ENOENT') throw error; }

// Activate only after all copies and the project path update have been verified.
await mkdir(dirname(pointer), { recursive: true });
try { await cp(pointer, join(target, 'storage-pointer.previous.json'), { errorOnExist: true, force: false }); }
catch (error) { if (error.code !== 'ENOENT') throw error; }
await writeFile(pointer + '.tmp', JSON.stringify({ version: 1, dataRoot: target }, null, 2), { mode: 0o600 });
await rename(pointer + '.tmp', pointer);
console.log(JSON.stringify({ dataRoot: target, verifiedFiles, relocatedFolders, originalFilesRetained: true }));
