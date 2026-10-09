import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile, copyFile, readdir, stat } from 'node:fs/promises';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const sourceRoot = join(repositoryRoot, 'apps/resource-service');
const argumentsList = process.argv.slice(2);
const option = name => { const index = argumentsList.indexOf(name); return index < 0 ? undefined : argumentsList[index + 1]; };
const runtimeIdentifier = option('--runtime') ?? 'win-x64';
const target = { 'win-x64': 'x86_64-pc-windows-msvc', 'win-arm64': 'aarch64-pc-windows-msvc' }[runtimeIdentifier];
const cacheDirectory = resolve(option('--cache') ?? join(repositoryRoot, 'artifacts/runtime/resources', runtimeIdentifier));
const cargoPath = option('--cargo') ?? 'cargo';
const isRequired = argumentsList.includes('--required');
const executable = join(cacheDirectory, 'kynxa-resource-service.exe');
const markerPath = join(cacheDirectory, 'build.json');

function runCargo(argumentsForCargo, { outputLimitBytes = 8 * 1024 * 1024 } = {}) {
  const inheritedFlags = process.env.CARGO_ENCODED_RUSTFLAGS ? process.env.CARGO_ENCODED_RUSTFLAGS.split('\x1f') :
    (process.env.RUSTFLAGS ?? '').split(/\s+/u).filter(Boolean);
  return new Promise((resolveResult, rejectResult) => {
    const child = spawn(cargoPath, argumentsForCargo, { windowsHide: true, shell: false,
      stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env,
        // MSVC CRT is included statically; users need neither Rust nor a Python installation.
        // 静态链接 MSVC CRT；最终用户无需安装 Rust 或 Python。
        CARGO_ENCODED_RUSTFLAGS: [...inheritedFlags, '-C', 'target-feature=+crt-static',
          `--remap-path-prefix=${repositoryRoot}=/kynxa`].join('\x1f') } });
    const stdout = [], stderr = []; let bytes = 0, overflow = false, timedOut = false;
    const timer = setTimeout(() => { timedOut = true; child.kill(); }, 300_000);
    for (const [stream, collected] of [[child.stdout, stdout], [child.stderr, stderr]]) stream.on('data', data => {
      bytes += data.length;
      if (bytes > outputLimitBytes) { overflow = true; child.kill(); }
      else collected.push(data);
    });
    child.once('error', error => { clearTimeout(timer); rejectResult(error); });
    child.once('close', exitCode => {
      clearTimeout(timer);
      if (exitCode !== 0 || overflow || timedOut) {
        const error = new Error(timedOut ? 'Resource runtime build timed out.' : overflow ? 'Resource build output exceeded its limit.'
          : `Resource runtime build failed: ${Buffer.concat(stderr).toString('utf8').slice(-4096)}`);
        error.code = timedOut ? 'RESOURCE_BUILD_TIMEOUT' : 'RESOURCE_BUILD_FAILED'; rejectResult(error);
      } else resolveResult(Buffer.concat(stdout).toString('utf8'));
    });
  });
}

async function sourceFingerprint() {
  const names = ['Cargo.toml', 'Cargo.lock', ...(await readdir(join(sourceRoot, 'src'))).filter(name => name.endsWith('.rs')).sort().map(name => `src/${name}`)];
  const hash = createHash('sha256').update(`${target}|${process.platform}|${process.arch}|static-msvc-crt-v1`)
    .update(await readFile(fileURLToPath(import.meta.url)));
  for (const name of names) hash.update(name).update(await readFile(join(sourceRoot, name)));
  return hash.digest('hex');
}

function allowedPackagePath(path) {
  return path === 'kynxa-resource-service.exe' || path === 'licenses/THIRD-PARTY-NOTICES.txt' ||
    /^licenses\/[A-Za-z0-9_-]+-[A-Za-z0-9._-]+\/(?:LICEN[CS]E(?:[-_.][A-Za-z0-9_-]+)*|COPYING)$/iu.test(path);
}

async function packageManifest(files) {
  const manifest = [];
  for (const path of files) {
    const packagePath = relative(cacheDirectory, path).replaceAll('\\', '/');
    if (!allowedPackagePath(packagePath)) throw new Error('Resource bundle contains an unsupported path.');
    manifest.push({ path: packagePath, sha256: createHash('sha256').update(await readFile(path)).digest('hex') });
  }
  return manifest;
}

async function validCachedPackage(previous, sourceHash) {
  if (previous?.schemaVersion !== 2 || previous.sourceHash !== sourceHash ||
      !Array.isArray(previous.files) || previous.files.length < 2 || previous.files.length > 256 ||
      previous.files[0]?.path !== 'kynxa-resource-service.exe' ||
      !previous.files.some(file => file.path === 'licenses/THIRD-PARTY-NOTICES.txt')) return false;
  try {
    const lines = (await readFile(join(cacheDirectory, 'bundle-files.txt'), 'utf8')).trim().split(/\r?\n/u);
    if (new Set(lines).size !== lines.length || JSON.stringify(lines) !== JSON.stringify(previous.files.map(file => file.path))) return false;
    for (const file of previous.files) {
      if (!allowedPackagePath(file.path) || typeof file.sha256 !== 'string' || !/^[a-f0-9]{64}$/u.test(file.sha256)) return false;
      if (createHash('sha256').update(await readFile(join(cacheDirectory, file.path))).digest('hex') !== file.sha256) return false;
    }
    return true;
  } catch { return false; }
}

async function collectDependencyNotices() {
  const metadata = JSON.parse(await runCargo(['metadata', '--manifest-path', join(sourceRoot, 'Cargo.toml'), '--locked', '--format-version', '1', '--filter-platform', target]));
  const files = [];
  const notice = ['KYNXA resource runtime third-party dependencies', '第三方依赖清单与许可证原文', ''];
  for (const dependency of metadata.packages.filter(value => value.source).sort((first, second) => first.name.localeCompare(second.name))) {
    notice.push(`${dependency.name} ${dependency.version}: ${dependency.license ?? 'see license file'}`);
    const packageRoot = dirname(dependency.manifest_path);
    const licenseFiles = (await readdir(packageRoot)).filter(name => /^(?:licen[cs]e(?:[-_.].*)?|copying)$/iu.test(name));
    if (dependency.license_file) licenseFiles.push(dependency.license_file);
    for (const name of new Set(licenseFiles)) {
      const original = resolve(packageRoot, name);
      if (!(await stat(original)).isFile()) continue;
      const destination = join(cacheDirectory, 'licenses', `${dependency.name}-${dependency.version}`, name.replaceAll('\\', '/').split('/').at(-1));
      await mkdir(dirname(destination), { recursive: true }); await copyFile(original, destination); files.push(destination);
    }
    if (!licenseFiles.length) throw new Error(`No redistribution license found for ${dependency.name}.`);
  }
  const noticePath = join(cacheDirectory, 'licenses', 'THIRD-PARTY-NOTICES.txt');
  await writeFile(noticePath, `${notice.join('\n')}\n`, 'utf8'); files.push(noticePath);
  return files;
}

async function buildRuntime() {
  if (!target) throw new Error('Unsupported resource runtime identifier.');
  await mkdir(cacheDirectory, { recursive: true });
  const sourceHash = await sourceFingerprint();
  let previous;
  try { previous = JSON.parse(await readFile(markerPath, 'utf8')); } catch { /* Missing cache is rebuilt. 缺少缓存时重新构建。 */ }
  if (await validCachedPackage(previous, sourceHash)) return;
  await runCargo(['build', '--manifest-path', join(sourceRoot, 'Cargo.toml'), '--release', '--locked', '--target', target,
    '--target-dir', join(cacheDirectory, 'build')]);
  await copyFile(join(cacheDirectory, 'build', target, 'release', 'kynxa-resource-service.exe'), executable);
  const notices = await collectDependencyNotices();
  const files = await packageManifest([executable, ...notices]);
  await writeFile(markerPath, `${JSON.stringify({ schemaVersion: 2, sourceHash, runtimeIdentifier, files }, null, 2)}\n`, 'utf8');
  await writeFile(join(cacheDirectory, 'bundle-files.txt'), `${files.map(file => file.path).join('\n')}\n`, 'utf8');
}

try { await buildRuntime(); process.stdout.write('Resource runtime ready.\n'); }
catch (error) {
  // A stale executable may remain for diagnosis, but a failed build never advertises it for publishing.
  // 保留旧可执行文件便于诊断；构建失败时清空本工具拥有的清单，禁止打包旧版本。
  await mkdir(cacheDirectory, { recursive: true });
  await writeFile(join(cacheDirectory, 'bundle-files.txt'), '', 'utf8');
  if (isRequired) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
  else {
    // Developer machines without Cargo keep existing CPU functionality; Release callers can require the Rust bundle.
    // 缺少 Cargo 的开发机保持原有 CPU 能力；发布调用方可以显式要求完整 Rust 包。
    process.stderr.write(`Resource runtime unavailable (${error.code ?? 'RESOURCE_BUILD_FAILED'}); conservative CPU fallback remains available.\n`);
  }
}
