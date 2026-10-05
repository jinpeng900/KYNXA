import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { randomUUID } from 'node:crypto';
import { test } from 'node:test';
import { toolFixture } from './tool-fixture.mjs';
import { HostTerminalRunner } from '../tools/host-terminal-runner.mjs';
import { ModelStore } from '../models/store.mjs';
import { ModelRuntime } from '../orchestration/runtime.mjs';
import { createModelServer } from '../server.mjs';
import { readSse } from '../models/streaming.mjs';

async function listen(server) {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return `http://127.0.0.1:${server.address().port}`;
}
async function close(server) {
  server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
}

for (const protocol of ['openai-completions', 'openai-responses', 'anthropic-messages'])
  test(`${protocol}: actual host output passes through the HTTP loop before the final answer, with one archived execution`,
    { skip: process.platform !== 'win32', timeout: 25000 }, async t => {
      const f = await toolFixture(t, { hostTerminalRunner: new HostTerminalRunner() });
      let rounds = 0;
      const upstream = createServer(async (request, response) => {
        try {
          let raw = ''; for await (const chunk of request) raw += chunk;
          const body = JSON.parse(raw), callId = 'terminal-live-http';
          rounds++;
          let value;
          if (rounds === 1) {
            const tool = body.tools.find(item => (item.description ?? item.function?.description ?? '').startsWith('terminal.host.run:'));
            assert.ok(tool, 'the host tool is selected for the actual terminal request');
            const name = tool.name ?? tool.function.name;
            const args = { shell: 'powershell', script: "[Console]::WriteLine('HTTP_LIVE_FIRST_中文');Start-Sleep -Milliseconds 250;[Console]::WriteLine('HTTP_LIVE_DONE')",
              reason: 'Run only the synthetic temporary HTTP fixture command.' };
            if (protocol === 'anthropic-messages') value = { stop_reason: 'tool_use', content: [
              { type: 'text', text: 'Run the synthetic command.' }, { type: 'tool_use', id: callId, name, input: args }] };
            else if (protocol === 'openai-responses') value = { status: 'completed', output: [
              { type: 'function_call', id: 'function-live-http', call_id: callId, name, arguments: JSON.stringify(args) }] };
            else value = { choices: [{ finish_reason: 'tool_calls', message: { role: 'assistant', content: 'Run the synthetic command.',
              tool_calls: [{ id: callId, type: 'function', function: { name, arguments: JSON.stringify(args) } }] } }] };
          } else {
            assert.equal(rounds, 2); assert.match(JSON.stringify(body.messages ?? body.input), /HTTP_LIVE_DONE/);
            if (protocol === 'anthropic-messages') value = { stop_reason: 'end_turn', content: [{ type: 'text', text: 'Done.' }] };
            else if (protocol === 'openai-responses') value = { status: 'completed', output: [{ type: 'message', role: 'assistant',
              content: [{ type: 'output_text', text: 'Done.' }] }] };
            else value = { choices: [{ finish_reason: 'stop', message: { role: 'assistant', content: 'Done.' } }] };
          }
          response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(value));
        } catch (error) { response.writeHead(500, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ error: error.message })); }
      });
      const upstreamUrl = await listen(upstream); t.after(() => close(upstream));
      const models = new ModelStore({ dataHome: f.dataHome });
      await models.save({ providerId: 'terminal-http-fixture', displayName: 'Synthetic terminal fixture', protocol,
        baseUrl: upstreamUrl + '/v1', models: ['model'], contextWindowTokens: 32768, maxOutputTokens: 2048 });
      const runtime = new ModelRuntime({ modelStore: models, dataHome: f.dataHome, conversationStore: f.conversations, toolService: f.service });
      const gateway = createModelServer({ modelStore: models, modelRuntime: runtime }), address = await listen(gateway);
      t.after(async () => { await gateway.shutdownModelRuntime(); await close(gateway); });
      const requestId = randomUUID();
      const response = await fetch(address + '/api/chat/stream', { method: 'POST', body: JSON.stringify({
        conversationId: f.conversationId, requestId, userMessageId: randomUUID(), provider: 'terminal-http-fixture',
        model: 'model', message: '调用本机终端执行测试命令', permissionMode: 'full' }) });
      assert.equal(response.status, 200);
      const events = []; for await (const frame of readSse(response.body)) events.push(JSON.parse(frame.data));
      const live = events.filter(item => item.type === 'terminal_output');
      assert.ok(live.length >= 2, JSON.stringify(events));
      for (const [index, item] of live.entries()) {
        assert.equal(item.conversationId, f.conversationId); assert.equal(item.requestId, requestId);
        assert.equal(item.terminal.toolCallId, 'terminal-live-http'); assert.equal(item.terminal.sequence, index + 1);
      }
      assert.match(live.map(item => item.terminal.text).join(''), /HTTP_LIVE_FIRST_中文.*HTTP_LIVE_DONE/s);
      const start = events.findIndex(item => item.type === 'tool_call'), finish = events.findIndex(item => item.type === 'tool_result');
      assert.ok(events.indexOf(live[0]) > start && events.indexOf(live.at(-1)) < finish);
      assert.equal(events.at(-1).type, 'completed'); assert.equal(events.at(-1).content, 'Done.');
      assert.equal(rounds, 2);
      const saved = (await f.conversations.readMessages(f.conversationId)).find(item => item.Id === requestId);
      assert.equal(saved.Content, 'Done.'); assert.equal(saved.ToolActivities.length, 1);
      assert.equal(saved.ToolActivities[0].status, 'completed'); assert.ok(saved.ToolActivities[0].resultRef);
      assert.equal(saved.ToolRun.diagnostics.executedToolCalls, 1);
    });
