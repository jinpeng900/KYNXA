import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, readFile, access, symlink, link, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { SandboxRunner } from '../../apps/model-gateway/tools/sandbox-runner.mjs';

test('real Windows AppContainer: snapshot IO, denied external read/network, process tree and bounded output',
  { skip: process.platform !== 'win32', timeout: 60000 }, async () => {
    const fixture = await mkdtemp(join(tmpdir(), 'kynxa-sandbox-smoke-'));
    const workspace = join(fixture, 'workspace');
    const outside = join(fixture, 'outside');
    const configuredData = join(workspace, 'custom-private-store');
    let onStarted;
    const toolHostPath = process.env.KYNXA_SANDBOX_SMOKE_TOOL_HOST;
    const runner = new SandboxRunner({ toolHostPath, excludedRoots: [configuredData], onStarted: event => onStarted?.(event) });
    const stages = [];
    let httpServer;
    try {
      await mkdir(workspace);
      await mkdir(outside);
      await mkdir(configuredData);
      await mkdir(join(workspace, 'Data'));
      await writeFile(join(workspace, 'readme.txt'), 'fixture before');
      await writeFile(join(outside, 'outside.txt'), 'FAKE_EXCLUDED_TEST_VALUE');
      await writeFile(join(workspace, '.env'), 'FAKE_ONLY=not-a-real-credential');
      await writeFile(join(workspace, 'Data', 'sample.json'), '{"fake":true}');
      await writeFile(join(configuredData, 'fixture.txt'), 'not copied');
      await symlink(outside, join(workspace, 'linked-outside'), 'junction');
      await link(join(outside, 'outside.txt'), join(workspace, 'linked-file.txt'));
      const capabilities = await runner.capabilities();
      assert.equal(capabilities.available, true, capabilities.reason);
      assert.equal(capabilities.network, false);
      assert.equal(capabilities.failClosed, true);
      assert.equal(runner.verifiedAppContainer, true);
      assert.deepEqual(capabilities.commands, ['node', 'cmd']);

      const run = async (args, timeoutMs = 10000, command = 'node') => {
        const result = await runner.run({ workspaceRoot: workspace, command, args, timeoutMs });
        stages.push(result.stagingDirectory);
        assert.equal(result.sandbox, 'appcontainer');
        assert.equal(result.tokenVerified, true);
        assert.equal(result.activeProcessesAfterExit, 0);
        return result;
      };
      const io = await run(['-e', `const fs=require('node:fs');fs.writeFileSync('readme.txt','sandbox changed');fs.writeFileSync('created.txt','staged output');console.log(JSON.stringify({content:fs.readFileSync('readme.txt','utf8'),env:process.env.KYNXA_SANDBOX,excluded:['.env','Data','custom-private-store','linked-outside','linked-file.txt'].every(p=>!fs.existsSync(p)),cwd:process.cwd()}));`]);
      assert.equal(io.exitCode, 0, io.stderr);
      const observedIo = JSON.parse(io.stdout);
      assert.equal(observedIo.content, 'sandbox changed');
      assert.equal(observedIo.env, 'appcontainer');
      assert.equal(observedIo.excluded, true);
      assert.equal(await readFile(join(workspace, 'readme.txt'), 'utf8'), 'fixture before');
      await assert.rejects(access(join(workspace, 'created.txt')));
      assert.equal(await readFile(join(io.stagingDirectory, 'created.txt'), 'utf8'), 'staged output');
      console.log('PASS: native token, snapshot write, original untouched, sensitive directories and junction omitted');

      await writeFile(join(workspace, 'entry.cjs'), `console.log(JSON.stringify({arguments:process.argv.slice(2),leaked:process.env.KYNXA_SANDBOX_SMOKE_FAKE_ENV??null}));`);
      process.env.KYNXA_SANDBOX_SMOKE_FAKE_ENV = 'FAKE_ONLY_NO_REAL_CREDENTIAL';
      const complexArgs = ['中文与 spaces', 'quotes " here', 'tail\\', 'two\\\\slashes', 'line\nnext'];
      let script;
      try { script = await run(['entry.cjs', ...complexArgs]); }
      finally { delete process.env.KYNXA_SANDBOX_SMOKE_FAKE_ENV; }
      assert.equal(script.exitCode, 0, script.stderr);
      assert.deepEqual(JSON.parse(script.stdout).arguments, complexArgs);
      assert.equal(JSON.parse(script.stdout).leaked, null);
      console.log('PASS: snapshot script entry, Unicode/quoted argv and no inherited host environment secret');

      const external = await run(['-e', `const fs=require('node:fs');try{fs.readFileSync(process.argv[1],'utf8');console.log('UNEXPECTED_READ');process.exitCode=3;}catch(e){console.log(JSON.stringify({denied:true,code:e.code}));}`, join(outside, 'outside.txt')]);
      assert.equal(external.exitCode, 0, external.stderr);
      assert.equal(JSON.parse(external.stdout).denied, true);
      assert.match(JSON.parse(external.stdout).code, /EACCES|EPERM/);
      console.log('PASS: external temporary fixture file read denied by Windows');

      let requests = 0;
      httpServer = createServer((request, response) => { requests++; response.end('fixture'); });
      await new Promise(resolve => httpServer.listen(0, '127.0.0.1', resolve));
      const port = httpServer.address().port;
      const network = await run(['-e', `require('node:http').get('http://127.0.0.1:${port}',r=>{console.log('UNEXPECTED_NETWORK');r.resume();process.exitCode=3;}).on('error',e=>console.log(JSON.stringify({denied:true,code:e.code})));`]);
      assert.equal(network.exitCode, 0, network.stderr);
      assert.equal(JSON.parse(network.stdout).denied, true);
      assert.equal(requests, 0);
      console.log('PASS: loopback request denied, host fixture server received no request');

      const cmdEcho = await run(['/d', '/c', 'echo sandbox-echosuccess'], 10000, 'cmd');
      assert.equal(cmdEcho.exitCode, 0, cmdEcho.stderr + '\n' + cmdEcho.stdout);
      assert.match(cmdEcho.stdout, /sandbox-echosuccess/);
      console.log('PASS: AppContainer cmd echo');
      const cmd = await run(['/d', '/c', 'echo sandbox-cmd>readme.txt & echo staged>cmd-created.txt'], 10000, 'cmd');
      assert.equal(cmd.exitCode, 0, cmd.stderr + '\n' + cmd.stdout);
      assert.match(await readFile(join(cmd.stagingDirectory, 'readme.txt'), 'utf16le'), /sandbox-cmd/);
      assert.equal(await readFile(join(workspace, 'readme.txt'), 'utf8'), 'fixture before');
      await assert.rejects(access(join(workspace, 'cmd-created.txt')));
      console.log('PASS: AppContainer cmd stage redirection, original unchanged');
      const cmdDir = await run(['/d', '/c', 'dir /b .'], 10000, 'cmd');
      if (cmdDir.exitCode === 0) assert.match(cmdDir.stdout, /readme\.txt/);
      else assert.ok(cmdDir.stderr.length > 0);
      console.log(`INFO: cmd DIR exit=${cmdDir.exitCode}; volume metadata can be denied by AppContainer, filesystem.list is the supported listing tool`);
      const cmdType = await run(['/d', '/c', 'type readme.txt'], 10000, 'cmd.exe');
      assert.equal(cmdType.exitCode, 0, cmdType.stderr);
      assert.match(cmdType.stdout, /fixture before/);
      const cmdOutside = await run(['/d', '/c', `type "${join(outside, 'outside.txt')}"`], 10000, 'cmd');
      assert.notEqual(cmdOutside.exitCode, 0);
      assert.doesNotMatch(cmdOutside.stdout, /FAKE_EXCLUDED_TEST_VALUE/);
      console.log('PASS: native cmd type/echo and staged write, original unchanged, outside TYPE denied');

      await writeFile(join(workspace, 'network.cjs'), `require('node:http').get('http://127.0.0.1:${port}',r=>{require('node:fs').writeFileSync('cmd-network.json',JSON.stringify({denied:false}));r.resume();process.exitCode=3;}).on('error',e=>require('node:fs').writeFileSync('cmd-network.json',JSON.stringify({denied:true,code:e.code})));`);
      const cmdNetwork = await run(['/d', '/c', 'node.exe --preserve-symlinks --preserve-symlinks-main network.cjs'], 10000, 'cmd');
      assert.equal(cmdNetwork.exitCode, 0, cmdNetwork.stderr);
      assert.equal(JSON.parse(await readFile(join(cmdNetwork.stagingDirectory, 'cmd-network.json'), 'utf8')).denied, true);
      assert.equal(requests, 0);
      console.log('PASS: cmd-created Node child retained AppContainer network restriction');

      await writeFile(join(workspace, 'fixture.test.cjs'), `const test=require('node:test');const assert=require('node:assert/strict');test('native sandbox child',()=>assert.equal(2+3,5));`);
      const nodeTest = await run(['--test', '--test-isolation=none', 'fixture.test.cjs']);
      assert.equal(nodeTest.exitCode, 0, nodeTest.stderr + '\n' + nodeTest.stdout);
      assert.match(nodeTest.stdout, /pass 1/);
      console.log('PASS: node --test --test-isolation=none inside the native sandbox');

      const descendantsScript = `const cp=require('node:child_process');const child=cp.spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});child.on('spawn',()=>{const pids={root:process.pid,child:child.pid};require('node:fs').writeFileSync('ready.json',JSON.stringify(pids));console.log(JSON.stringify(pids));setInterval(()=>{},1000);});child.on('error',e=>{console.error(e.message);process.exit(3);});`;
      const timed = await run(['-e', descendantsScript], 600);
      assert.equal(timed.timedOut, true);
      const timedPids = JSON.parse(timed.stdout);
      assert.throws(() => process.kill(timedPids.root, 0));
      assert.throws(() => process.kill(timedPids.child, 0));
      console.log('PASS: timeout terminated the real parent and child processes');

      const normalTree = await run(['-e', `const child=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});child.on('spawn',()=>{console.log(JSON.stringify({child:child.pid}));child.unref();});`]);
      assert.equal(normalTree.exitCode, 0, normalTree.stderr);
      assert.throws(() => process.kill(JSON.parse(normalTree.stdout).child, 0));
      console.log('PASS: normal parent completion also ended an unreferenced real child');

      const controller = new AbortController();
      const started = new Promise(resolve => { onStarted = resolve; });
      const pendingCancellation = runner.run({ workspaceRoot: workspace, command: 'node', args: ['-e', descendantsScript], timeoutMs: 10000 }, controller.signal);
      // A native start record identifies the stage; wait until the actual child has also started.
      // 原生启动记录用于确定阶段；还需等待实际子进程启动。
      const cancellationStage = (await started).stagingDirectory;
      for (let attempt = 0; attempt < 200; attempt++) {
        try { await readFile(join(cancellationStage, 'ready.json')); break; }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      await access(join(cancellationStage, 'ready.json'));
      controller.abort();
      try {
        await assert.rejects(pendingCancellation, error => {
          assert.equal(error.name, 'AbortError');
          assert.equal(error.sandboxResult?.activeProcessesAfterExit, 0);
          assert.equal(error.sandboxResult?.cancelled, true);
          const pids = JSON.parse(error.sandboxResult.stdout);
          assert.throws(() => process.kill(pids.root, 0));
          assert.throws(() => process.kill(pids.child, 0));
          return true;
        });
      } finally { onStarted = undefined; }
      console.log('PASS: cancellation terminated and observed the real parent and child processes');

      await writeFile(join(workspace, 'tree.cjs'), descendantsScript);
      const cmdController = new AbortController();
      const cmdStarted = new Promise(resolve => { onStarted = resolve; });
      const pendingCmdCancellation = runner.run({ workspaceRoot: workspace, command: 'cmd',
        args: ['/d', '/c', 'node.exe --preserve-symlinks --preserve-symlinks-main tree.cjs'], timeoutMs: 10000 }, cmdController.signal);
      const cmdCancellationStage = (await cmdStarted).stagingDirectory;
      for (let attempt = 0; attempt < 200; attempt++) {
        try { await readFile(join(cmdCancellationStage, 'ready.json')); break; }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      await access(join(cmdCancellationStage, 'ready.json'));
      cmdController.abort();
      try {
        await assert.rejects(pendingCmdCancellation, error => {
          assert.equal(error.name, 'AbortError');
          assert.equal(error.sandboxResult?.activeProcessesAfterExit, 0);
          const pids = JSON.parse(error.sandboxResult.stdout);
          assert.throws(() => process.kill(error.sandboxResult.processId, 0));
          assert.throws(() => process.kill(pids.root, 0));
          assert.throws(() => process.kill(pids.child, 0));
          return true;
        });
      } finally { onStarted = undefined; }
      console.log('PASS: cancellation terminated the actual cmd -> Node -> grandchild tree');

      const spam = await run(['-e', `for(let i=0;i<10000;i++)process.stdout.write('x'.repeat(4096));`]);
      assert.equal(spam.outputTruncated, true);
      assert.ok(Buffer.byteLength(spam.stdout) + Buffer.byteLength(spam.stderr) <= 256 * 1024);
      console.log('PASS: output limit terminated the job and bounded the result');
      const memory = await run(['-e', `require('node:buffer').Buffer.alloc(400*1024*1024).fill(1);console.log('UNEXPECTED_ALLOCATION');`]);
      assert.notEqual(memory.exitCode, 0);
      assert.equal(memory.timedOut, false);
      assert.doesNotMatch(memory.stdout, /UNEXPECTED_ALLOCATION/);
      console.log('PASS: OS job memory limit prevented a 400 MiB committed allocation');

      const managedData = join(fixture, 'private-app-data');
      const managedWorkspace = join(managedData, 'Desktop', 'Projects', randomUUID().replaceAll('-', ''));
      await mkdir(managedWorkspace, { recursive: true });
      await mkdir(join(managedWorkspace, 'Models'));
      await mkdir(join(managedData, 'Chats'));
      await writeFile(join(managedWorkspace, 'note.txt'), 'managed work original');
      await writeFile(join(managedWorkspace, 'Models', 'fixture.txt'), 'fake model settings must be omitted');
      const managedRunner = new SandboxRunner({ toolHostPath, excludedRoots: [managedData] });
      await assert.rejects(managedRunner.run({ workspaceRoot: managedWorkspace, command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
      const managed = await managedRunner.run({ workspaceRoot: managedWorkspace, trustedManagedWorkspace: true, command: 'node',
        args: ['-e', `const fs=require('node:fs');console.log(JSON.stringify({value:fs.readFileSync('note.txt','utf8'),privatePresent:fs.existsSync('Models')}));fs.writeFileSync('note.txt','sandbox managed change');`] });
      try {
        assert.equal(managed.exitCode, 0, managed.stderr);
        assert.equal(JSON.parse(managed.stdout).value, 'managed work original');
        assert.equal(JSON.parse(managed.stdout).privatePresent, false);
        assert.equal(await readFile(join(managedWorkspace, 'note.txt'), 'utf8'), 'managed work original');
      } finally { await managedRunner.cleanupAll(); }
      await assert.rejects(managedRunner.run({ workspaceRoot: join(managedData, 'Chats'), trustedManagedWorkspace: true,
        command: 'node', args: ['-v'] }), { code: 'SANDBOX_INVALID_WORKSPACE' });
      console.log('PASS: trusted canonical managed work snapshot permitted, formal Data/Chats still rejected, original untouched');
      await assert.rejects(runner.run({ workspaceRoot: workspace, command: 'cmd.exe', args: ['/c', 'echo unsafe'] }), { code: 'SANDBOX_INVALID_REQUEST' });
      await assert.rejects(runner.run({ workspaceRoot: workspace, command: process.execPath, args: ['-v'] }), { code: 'SANDBOX_COMMAND_UNSUPPORTED' });
      await assert.rejects(runner.run({ workspaceRoot: workspace, command: 'node', args: ['--inspect', '-e', '1'] }), { code: 'SANDBOX_COMMAND_UNSUPPORTED' });
      const missing = new SandboxRunner({ toolHostPath: join(fixture, 'missing.exe') });
      assert.equal((await missing.capabilities()).available, false);
      assert.equal(missing.verifiedAppContainer, false);
      await assert.rejects(missing.run({ workspaceRoot: workspace, command: 'node', args: ['-v'] }), { code: 'SANDBOX_UNAVAILABLE' });
      await assert.rejects(runner.cleanup(workspace), { code: 'SANDBOX_INVALID_CLEANUP' });
      console.log('PASS: unsupported runtime, arbitrary executable, debugger and missing helper fail closed');
    } finally {
      if (httpServer) await new Promise(resolve => httpServer.close(resolve));
      for (const stage of stages) await runner.cleanup(stage);
      await rm(fixture, { recursive: true, force: true });
    }
  });
