import assert from 'node:assert/strict';
import { test } from 'node:test';
import { projectLocalToolCatalog, localToolGrammarRejection, readLocalToolGrammarRejection,
  planLocalToolGrammarRecovery } from '../models/local-tool-compatibility.mjs';
import { builtinDescriptors } from '../official-tools/Tools/catalog.mjs';
import { wireCatalog, toolDeclarations, decodeToolTurn } from '../models/tool-protocols.mjs';
import { validateBuiltinInput } from '../tools/tool-input-validation.mjs';

const connection = { providerId: 'qwen-local', baseUrl: 'http://127.0.0.1:8080/v1', models: ['qwen-synthetic'] };
const localModel = { backend: 'llama.cpp', source: 'llama-cpp-props', observationOnly: true,
  endpointOrigin: 'http://127.0.0.1:8080' };
const options = { connection, localModel };
const rejectionBody = { error: { code: 400, type: 'invalid_request_error',
  message: 'Failed to initialize samplers: failed to parse grammar' } };

test('local llama declarations retain every enabled tool and wire identity while removing finite repetition limits', () => {
  const catalog = wireCatalog(builtinDescriptors), before = JSON.stringify(catalog);
  const projection = projectLocalToolCatalog(catalog, options);
  assert.equal(projection.applied, true);
  assert.equal(projection.catalog.length, catalog.length);
  assert.ok(projection.omittedConstraintCount > 0);
  assert.equal(JSON.stringify(catalog), before, 'authoritative descriptors remain immutable');
  for (const [index, descriptor] of catalog.entries()) {
    const projected = projection.catalog[index];
    assert.equal(projected.name, descriptor.name);
    assert.equal(projected.wireName, descriptor.wireName);
    assert.deepEqual(projected.inputSchema.required, descriptor.inputSchema.required);
    assert.equal(projected.inputSchema.additionalProperties, descriptor.inputSchema.additionalProperties);
  }
  const terminal = projection.catalog.find(item => item.name === 'terminal.host.run');
  assert.ok(terminal, 'real host tool remains declared');
  assert.equal(terminal.inputSchema.properties.script.maxLength, undefined);
  assert.deepEqual(terminal.inputSchema.properties.shell.enum, ['cmd', 'powershell']);
  const loading = projection.catalog.find(item => item.name === 'tool.load');
  assert.equal(loading.inputSchema.properties.names.maxItems, undefined);
  for (const protocol of ['openai-chat', 'openai-responses', 'anthropic-messages']) {
    const declaration = toolDeclarations(protocol, projection.catalog)[0];
    assert.equal(declaration.function?.name ?? declaration.name, catalog[0].wireName);
  }
});

test('projection never weakens original executor limits, required fields, enum or type validation', () => {
  const descriptor = builtinDescriptors.find(item => item.name === 'terminal.host.run');
  projectLocalToolCatalog(wireCatalog([descriptor]), options);
  const valid = { shell: 'cmd', script: 'echo synthetic', reason: 'synthetic fixture' };
  assert.doesNotThrow(() => validateBuiltinInput(descriptor, valid));
  assert.throws(() => validateBuiltinInput(descriptor, { ...valid, script: 'x'.repeat(16385) }));
  assert.throws(() => validateBuiltinInput(descriptor, { ...valid, shell: 'unknown' }));
  assert.throws(() => validateBuiltinInput(descriptor, { ...valid, script: 7 }));
  const loading = builtinDescriptors.find(item => item.name === 'tool.load');
  assert.throws(() => validateBuiltinInput(loading, {}));
  assert.throws(() => validateBuiltinInput(loading, { names: Array.from({ length: 33 }, () => 'filesystem.read') }));
});

test('a projected alias still decodes against the original request catalog', () => {
  const catalog = wireCatalog(builtinDescriptors.filter(item => item.name === 'tool.load'));
  const projection = projectLocalToolCatalog(catalog, options);
  const turn = decodeToolTurn('openai-chat', { choices: [{ message: { role: 'assistant', content: '', tool_calls: [{
    id: 'synthetic-call', type: 'function', function: { name: projection.catalog[0].wireName,
      arguments: JSON.stringify({ names: ['filesystem.read'] }) }
  }] }, finish_reason: 'tool_calls' }] }, catalog);
  assert.equal(turn.calls[0].name, catalog[0].name);
  assert.deepEqual(turn.calls[0].arguments, { names: ['filesystem.read'] });
});

test('schema projection visits refs and compound schemas without altering literal values or prototype-shaped fields', () => {
  const literal = { maxLength: 5000, maxItems: 9000 };
  const schema = JSON.parse('{"type":"object","properties":{"__proto__":{"type":"string","maxLength":5000}},"required":["__proto__"],"additionalProperties":false}');
  schema.$defs = { note: { type: 'string', maxLength: 2000, minLength: 1000, enum: ['fixed'] } };
  schema.properties.notes = { anyOf: [{ type: 'array', items: { $ref: '#/$defs/note' }, maxItems: 32 }, { type: 'null' }] };
  schema.examples = [literal];
  schema.default = literal;
  const catalog = [{ name: 'synthetic.notes', wireName: 'synthetic_notes', inputSchema: schema }];
  const projected = projectLocalToolCatalog(catalog, options).catalog[0].inputSchema;
  assert.equal(projected.$defs.note.maxLength, undefined);
  assert.equal(projected.$defs.note.minLength, undefined);
  assert.deepEqual(projected.$defs.note.enum, ['fixed']);
  assert.equal(projected.properties.notes.anyOf[0].maxItems, undefined);
  assert.equal(projected.properties.notes.anyOf[0].items.$ref, '#/$defs/note');
  assert.equal(projected.properties.__proto__.type, 'string');
  assert.equal(Object.getPrototypeOf(projected.properties), Object.prototype);
  assert.deepEqual(projected.examples, [literal]);
  assert.deepEqual(projected.default, literal);
});

test('Qwen names, provider IDs, ports and untrusted local metadata cannot activate compatibility', () => {
  const catalog = wireCatalog(builtinDescriptors);
  for (const untrusted of [
    { connection, localModel: {} },
    { connection, localModel: { ...localModel, backend: 'ollama' } },
    { connection, localModel: { ...localModel, source: 'configured' } },
    { connection, localModel: { ...localModel, endpointOrigin: 'http://127.0.0.1:9999' } },
    { connection: { ...connection, baseUrl: 'https://api.example.com/v1' }, localModel },
    { connection: { ...connection, baseUrl: 'http://192.168.1.5:8080/v1' }, localModel },
    { connection: { ...connection, baseUrl: 'http://user:secret@127.0.0.1:8080/v1' }, localModel }
  ]) {
    const result = projectLocalToolCatalog(catalog, untrusted);
    assert.equal(result.applied, false);
    assert.equal(result.catalog, catalog);
  }
});

test('only explicit HTTP400 grammar errors produce a sanitized rejection marker', async () => {
  const marker = localToolGrammarRejection(rejectionBody, { status: 400 });
  assert.deepEqual(marker, { kind: 'tool-grammar', code: 'MODEL_LOCAL_TOOL_GRAMMAR_REJECTED' });
  assert.equal(localToolGrammarRejection(rejectionBody, { status: 500 }), null);
  for (const message of ['context length exceeded', 'unknown model', 'invalid tool arguments',
    'grammar field is not supported', 'invalid API key']) {
    assert.equal(localToolGrammarRejection({ error: { message } }, { status: 400 }), null);
  }
  const read = await readLocalToolGrammarRejection(new Response(JSON.stringify(rejectionBody), { status: 400 }));
  assert.deepEqual(read, marker);
  assert.equal(await readLocalToolGrammarRejection(new Response('private reflected data'.repeat(10_000), { status: 400 })), null);
  assert.equal(await readLocalToolGrammarRejection(new Response('not JSON', { status: 400 })), null);
});

test('grammar recovery changes one failed model step once while retaining fields and rejecting side-effect replay', () => {
  const catalog = wireCatalog([{ name: 'synthetic.tool', inputSchema: { type: 'object', required: ['value'],
    additionalProperties: false, properties: { value: { type: 'string', minLength: 1, maxLength: 2000,
      pattern: '^.{1,2000}$', enum: ['valid'] }, amount: { type: 'integer', minimum: 1, maximum: 1000 } } } }]);
  const bounded = projectLocalToolCatalog(catalog, options);
  const rejection = localToolGrammarRejection(rejectionBody, { status: 400 });
  const recovery = planLocalToolGrammarRecovery({ ...options, catalog: bounded.catalog, rejection });
  assert.equal(recovery.recoveryAttempts, 1);
  assert.equal(recovery.mode, 'structural');
  assert.equal(recovery.catalog.length, 1);
  assert.equal(recovery.catalog[0].wireName, catalog[0].wireName);
  assert.deepEqual(recovery.catalog[0].inputSchema.required, ['value']);
  assert.deepEqual(recovery.catalog[0].inputSchema.properties.value.enum, ['valid']);
  assert.equal(recovery.catalog[0].inputSchema.properties.value.pattern, undefined);
  assert.equal(recovery.catalog[0].inputSchema.properties.amount.type, 'integer');
  assert.equal(recovery.catalog[0].inputSchema.properties.amount.maximum, undefined);
  for (const disallowed of [{ recoveryAttempts: 1 }, { hasVisibleOutput: true }, { executedToolCalls: 1 },
    { rejection: null }, { localModel: {} }]) {
    assert.equal(planLocalToolGrammarRecovery({ ...options, catalog: bounded.catalog, rejection, ...disallowed }), null);
  }
  assert.equal(planLocalToolGrammarRecovery({ ...options, catalog: recovery.catalog, rejection }), null,
    'an unchanged structural schema never creates another retry');
  assert.equal(planLocalToolGrammarRecovery({ ...options, catalog: [], rejection }), null);
});
