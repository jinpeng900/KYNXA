import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../app.js", import.meta.url), "utf8");
const provider = (providerId, displayName = "Fixture connection") => ({
  providerId, displayName, baseUrl: "http://127.0.0.1:9000/v1", models: ["fixture-model"],
  protocol: "openai-completions", hasApiKey: false
});
const deferred = () => {
  let resolve;
  const promise = new Promise(value => { resolve = value; });
  return { promise, resolve };
};

// Execute the actual editor functions and handlers with a small form fixture.
// No browser, gateway, saved configuration, or network request is involved.
function editor({ catalog = [], refresh = async () => catalog } = {}) {
  const fields = new Map(["providerId", "displayName", "baseUrl", "models", "protocol", "apiKey"]
    .map(name => [name, { value: "", disabled: false, readOnly: false }]));
  const handlers = new Map();
  const form = {
    elements: { namedItem: name => fields.get(name) },
    querySelectorAll: () => [...fields.values()],
    addEventListener: (name, handler) => handlers.set(name, handler),
    reportValidity: () => true
  };
  const dialog = { open: false, showModal() { this.open = true; } };
  const status = { textContent: "", dataset: {} };
  const requests = [];
  let refreshes = 0;
  const context = vm.createContext({
    AbortController, AbortSignal, console,
    FormData: class { constructor() {} get(name) { return fields.get(name).value; } },
    $: selector => selector === "#modelForm" ? form : selector === "#modelDialog" ? dialog : status,
    closeMenu() {}, renderProviders() {},
    async refreshModels() {
      const result = await refresh(++refreshes);
      context.loadedProviders = result;
      vm.runInContext("providers = loadedProviders", context);
      return result;
    },
    async api(path, options) {
      const input = JSON.parse(options.body);
      requests.push({ path, input });
      return { provider: { ...input, hasApiKey: false } };
    }
  });
  context.loadedProviders = catalog;
  vm.runInContext(`let providers = loadedProviders;
    let modelBusy = false, modelOperation = null, testedRevision = null, formRevision = 0;
    let editingProviderId = null, newConnectionEdited = false, newProviderPrefix = "custom-api";`, context);
  for (const name of ["nextProviderId", "synchronizeNewProviderId", "formConnection", "setModelStatus", "setConnection", "openModels", "setModelBusy"]) {
    const match = source.match(new RegExp(`^(?:async )?function ${name}\\([^\\n]*\\) \\{[\\s\\S]*?^\\}`, "m"));
    if (match) vm.runInContext(match[0], context);
  }
  for (const name of ["input", "submit"]) {
    const match = source.match(new RegExp(`^\\$\\("#modelForm"\\)\\.addEventListener\\("${name}", [\\s\\S]*?^\\}\\);`, "m"));
    assert.ok(match, `The ${name} editor handler must exist.`);
    vm.runInContext(match[0], context);
  }
  return {
    fields, requests, status,
    open: () => vm.runInContext("openModels()", context),
    select: (item, editing = false) => {
      context.selectedProvider = item;
      context.editExisting = editing;
      vm.runInContext("setConnection(selectedProvider, editExisting)", context);
    },
    edit(values) {
      for (const [name, value] of Object.entries(values)) fields.get(name).value = value;
      handlers.get("input")();
    },
    save: () => handlers.get("submit")({ preventDefault() {} })
  };
}

test("a cold model dialog allocates an unused ID after loading saved connections", async () => {
  const fixture = editor({ refresh: async () => [provider("custom-api"), provider("custom-api-2")] });
  await fixture.open();
  assert.equal(fixture.fields.get("providerId").value, "custom-api-3");
  assert.equal(fixture.fields.get("providerId").readOnly, false);
});

test("a late catalog preserves every field once a new form has been edited", async () => {
  const pending = deferred();
  const fixture = editor({ refresh: () => pending.promise });
  const opening = fixture.open();
  const input = { providerId: "custom-api", displayName: "Typed name", baseUrl: "http://127.0.0.1:8000/v1", models: "typed-model" };
  fixture.edit(input);
  pending.resolve([provider("custom-api")]);
  await opening;
  for (const [name, value] of Object.entries(input)) assert.equal(fixture.fields.get(name).value, value);
});

test("reopening an already edited new form retains its chosen ID", async () => {
  const fixture = editor({ refresh: async () => [provider("custom-api")] });
  fixture.select(null);
  fixture.edit({ providerId: "custom-api", displayName: "Unfinished fixture" });
  await fixture.open();
  assert.equal(fixture.fields.get("providerId").value, "custom-api");
  assert.equal(fixture.fields.get("displayName").value, "Unfinished fixture");
});

test("opening an existing connection does not replace its ID with a create ID", async () => {
  const existing = provider("custom-api");
  const fixture = editor({ catalog: [existing] });
  fixture.select(existing, true);
  await fixture.open();
  assert.equal(fixture.fields.get("providerId").value, existing.providerId);
  assert.equal(fixture.fields.get("providerId").readOnly, true);
});

test("saving a new connection checks the current catalog and refuses an existing ID", async () => {
  const fixture = editor({ refresh: async () => [provider("custom-api")] });
  fixture.select(null);
  fixture.edit({ displayName: "New connection", baseUrl: "http://127.0.0.1:8000/v1", models: "new-model" });
  await fixture.save();
  assert.equal(fixture.requests.length, 0, "An existing connection must never receive a create request.");
  assert.match(fixture.status.textContent, /ID.*已存在/);
  assert.equal(fixture.fields.get("displayName").value, "New connection");
  assert.equal(fixture.fields.get("providerId").value, "custom-api");
});

test("saving a new connection stops when the current catalog cannot be verified", async () => {
  const fixture = editor({ refresh: async () => { throw new Error("Fixture gateway unavailable"); } });
  fixture.select(null);
  fixture.edit({ displayName: "New connection", baseUrl: "http://127.0.0.1:8000/v1", models: "new-model" });
  await fixture.save();
  assert.equal(fixture.requests.length, 0);
  assert.match(fixture.status.textContent, /保存失败/);
});

test("a new connection with an unused ID saves its typed values", async () => {
  const fixture = editor({ catalog: [provider("existing")] });
  fixture.select(null);
  fixture.edit({ providerId: "new-fixture", displayName: "New fixture", baseUrl: "http://127.0.0.1:8000/v1", models: "first-model\nsecond-model" });
  await fixture.save();
  assert.equal(fixture.requests.length, 1);
  assert.equal(fixture.requests[0].input.providerId, "new-fixture");
  assert.equal(fixture.requests[0].input.displayName, "New fixture");
  assert.deepEqual(fixture.requests[0].input.models, ["first-model", "second-model"]);
});

test("editing an existing connection retains its ID and saves normally", async () => {
  const existing = provider("custom-api");
  const fixture = editor({ catalog: [existing] });
  fixture.select(existing, true);
  fixture.edit({ displayName: "Updated fixture" });
  await fixture.save();
  assert.equal(fixture.requests.length, 1);
  assert.equal(fixture.requests[0].input.providerId, existing.providerId);
  assert.equal(fixture.requests[0].input.displayName, "Updated fixture");
  assert.equal(fixture.fields.get("providerId").readOnly, true);
});

test("a preset creates a separate ID when that preset already exists", () => {
  const fixture = editor({ catalog: [provider("ollama")] });
  fixture.select(provider("ollama"));
  assert.equal(fixture.fields.get("providerId").value, "ollama-2");
  assert.equal(fixture.fields.get("providerId").readOnly, false);
});
