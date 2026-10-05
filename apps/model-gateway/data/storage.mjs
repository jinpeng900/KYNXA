import { readFileSync, existsSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, dirname, join, resolve, isAbsolute } from 'node:path';

export function conversationDataRoot(dataHome) {
  const path = resolve(dataHome);
  return basename(path).toLowerCase() === 'models' ? dirname(path) : join(path, 'Conversations');
}

export function modelHome(env = process.env, userHome = homedir()) {
  if (env.KYNXA_MODEL_HOME) return resolve(env.KYNXA_MODEL_HOME);
  let root = env.KYNXA_DATA_HOME;
  const pointer = join(userHome, '.kynxa', 'storage.json');
  if (!root && existsSync(pointer)) {
    root = JSON.parse(readFileSync(pointer, 'utf8').replace(/^\uFEFF/, '')).dataRoot;
    if (!root) throw new Error('KYNXA 存储目录配置为空。');
  }
  if (root && !isAbsolute(root)) throw new Error('KYNXA 存储目录必须为绝对路径。');
  return root ? join(resolve(root), 'Models') : join(userHome, '.kynxa', 'models');
}
