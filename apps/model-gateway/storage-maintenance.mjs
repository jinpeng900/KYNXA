import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

// The desktop owns this marker while it copies and verifies data. No HTTP mutation endpoint.
export function storageMigrationActive(userHome = homedir()) {
  try {
    const { pid } = JSON.parse(readFileSync(join(userHome, '.kynxa', 'storage-migration.lock'), 'utf8'));
    if (!Number.isInteger(pid) || pid <= 0) return true;
    try { process.kill(pid, 0); return true; }
    catch (error) { return error.code !== 'ESRCH'; }
  } catch (error) { return error.code !== 'ENOENT'; }
}
