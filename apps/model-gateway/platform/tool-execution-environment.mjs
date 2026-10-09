const EXECUTOR_KINDS = new Set(['unknown', 'builtin-tool', 'builtin-http', 'host-terminal', 'desktop-tool-host',
  'sandbox-terminal', 'mcp-stdio', 'mcp-http']);
const EXECUTOR_LOCATIONS = new Set(['unknown', 'gateway-host', 'remote-service']);
const OPERATION_LOCATIONS = new Set(['unknown', 'gateway-host']);
const LOCATION_SCOPES = new Set(['unknown', 'builtin-execution-policy', 'configured-transport-process', 'configured-service-endpoint']);
const REQUEST_ORIGINS = new Set(['unknown', 'gateway-host', 'tool-defined-unknown', 'unavailable']);
const BROWSER_LOCATIONS = new Set(['unknown', 'gateway-host', 'remote-browser']);
const BROWSER_EVIDENCE = new Set(['application-verified-browser-target', 'trusted-browser-configuration']);
const BROWSER_MODES = new Set(['existing-browser', 'independent-browser', 'remote-browser', 'custom-browser']);
const BROWSER_VISIBILITIES = new Set(['headless', 'visible']);

const isObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);

/** Normalize a small provenance contract; validation neither establishes trust nor grants execution permission.
 * 规范化小型执行来源合同；通过校验不等于来源可信，更不等于已授予执行权限。 */
export function normalizeToolExecutionEnvironment(input) {
  if (!isObject(input) || input.schemaVersion !== 1 || !EXECUTOR_KINDS.has(input.executorKind) ||
      !EXECUTOR_LOCATIONS.has(input.executorLocation) || !OPERATION_LOCATIONS.has(input.operationLocation) ||
      !LOCATION_SCOPES.has(input.locationScope) || input.gatewayHostMeaning !== 'machine-running-the-gateway' ||
      input.userDeviceRelationship !== 'unverified' || input.grantsPermission !== false || !isObject(input.network) ||
      !REQUEST_ORIGINS.has(input.network.requestOrigin) || input.network.egress !== 'unknown' ||
      input.network.proxy !== 'unknown' || input.network.sameEgressDoesNotProveSameMachine !== true) return undefined;
  const value = { schemaVersion: 1, executorKind: input.executorKind, executorLocation: input.executorLocation,
    operationLocation: input.operationLocation, locationScope: input.locationScope,
    gatewayHostMeaning: 'machine-running-the-gateway', userDeviceRelationship: 'unverified', grantsPermission: false,
    network: { requestOrigin: input.network.requestOrigin, egress: 'unknown', proxy: 'unknown',
      sameEgressDoesNotProveSameMachine: true } };
  if (input.isolation !== undefined) {
    if (input.isolation !== 'windows-appcontainer') return undefined;
    value.isolation = input.isolation;
  }
  if (input.sandboxVerified !== undefined) {
    if (typeof input.sandboxVerified !== 'boolean' || value.isolation !== 'windows-appcontainer') return undefined;
    value.sandboxVerified = input.sandboxVerified;
  }
  if (input.serviceMachineIdentity !== undefined) {
    if (input.serviceMachineIdentity !== 'unknown') return undefined;
    value.serviceMachineIdentity = 'unknown';
  }
  if (input.browserTarget !== undefined) {
    const browser = input.browserTarget;
    if (!isObject(browser) || !BROWSER_LOCATIONS.has(browser.location) || !BROWSER_EVIDENCE.has(browser.evidence) ||
        browser.mode !== undefined && !BROWSER_MODES.has(browser.mode) ||
        browser.visibility !== undefined && !BROWSER_VISIBILITIES.has(browser.visibility)) return undefined;
    value.browserTarget = { location: browser.location, evidence: browser.evidence,
      ...(browser.mode !== undefined ? { mode: browser.mode } : {}),
      ...(browser.visibility !== undefined ? { visibility: browser.visibility } : {}) };
    Object.freeze(value.browserTarget);
  }
  // Keep only fixed enums, never paths, addresses, credentials or arbitrary server text.
  // 仅保留固定枚举，不保留路径、地址、凭据或服务任意文本；大小上限也不构成当前请求授权。
  if (Buffer.byteLength(JSON.stringify(value), 'utf8') >= 1024) return undefined;
  Object.freeze(value.network);
  return Object.freeze(value);
}
