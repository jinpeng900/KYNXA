import { writeFile, rename } from 'node:fs/promises';
import { join } from 'node:path';

const target = process.argv[2];
await writeFile(join(target, 'initializer.pid.tmp'), String(process.pid));
await rename(join(target, 'initializer.pid.tmp'), join(target, 'initializer.pid'));
if (process.env.KYNXA_INITIALIZER_TEST_MODE === 'failure') {
  process.stderr.write('fixture initialization failure\n');
  process.exitCode = 2;
} else if (process.env.KYNXA_INITIALIZER_TEST_MODE === 'wait') {
  setInterval(() => {}, 1000);
} else {
  // Both pipes must drain without deadlocking while the parent waits for exit.
  // 父进程等待退出时，两条管道都必须排空，避免死锁。
  process.stdout.write('o'.repeat(128 * 1024));
  process.stderr.write('e'.repeat(128 * 1024));
  await writeFile(join(target, 'initialized.txt'), target);
}
