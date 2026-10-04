const reason = { type: 'string', minLength: 1, maxLength: 2000 };
const window = { windowId: { type: 'string', minLength: 1, maxLength: 20 },
  processId: { type: 'integer', minimum: 1, maximum: 2147483647 } };
const point = { x: { type: 'integer', minimum: 0, maximum: 32767 }, y: { type: 'integer', minimum: 0, maximum: 32767 } };
const target = ['windowId', 'processId'];
const region = { type: 'object', properties: { ...point,
  width: { type: 'integer', minimum: 1, maximum: 8192 }, height: { type: 'integer', minimum: 1, maximum: 8192 } },
  required: ['x', 'y', 'width', 'height'], additionalProperties: false };
export const computerKeyNames = ['ENTER', 'TAB', 'ESC', 'BACKSPACE', 'DELETE', 'UP', 'DOWN', 'LEFT', 'RIGHT', 'HOME', 'END',
  'PAGEUP', 'PAGEDOWN', 'CTRL+A', 'CTRL+C', 'CTRL+V', 'CTRL+S', 'CTRL+F', 'CTRL+L', 'CTRL+Z', 'CTRL+Y',
  'SHIFT+TAB', 'CTRL+ENTER', 'CTRL+PLUS', 'CTRL+MINUS', 'CTRL+0'];
function descriptor(action, description, properties = {}, required = []) {
  return { name: `computer.${action}`, source: 'builtin',
    description: `${description} Host desktop; broker permissions and target checks apply.`,
    inputSchema: { type: 'object', properties: { ...properties, reason }, required: [...required, 'reason'], additionalProperties: false } };
}

/** Installed descriptors only; host control stays behind the broker and native window checks. */
export const computerDescriptors = [
  descriptor('windows', 'List visible local windows with their windowId, processId, executable, client dimensions and isResponding; never invent target IDs.',
    { processId: window.processId }),
  descriptor('apps', 'List known local executable applications from Windows App Paths and system application entries. No disk scan.'),
  descriptor('screenshot', 'Capture the identified client area, optionally crop original physical pixels without resizing. PNG is archived locally; the text projection does not provide image vision.', { ...window, crop: region }, target),
  descriptor('read', 'Bounded visible UIA text/element coordinates. Not browser DOM or background tabs. Password name/value/text are excluded.',
    { ...window, maxCharacters: { type: 'integer', minimum: 1, maximum: 64000 }, maxElements: { type: 'integer', minimum: 1, maximum: 1000 },
      timeoutMs: { type: 'integer', minimum: 500, maximum: 5000 }, region,
      elementId: { type: 'string', minLength: 1, maxLength: 256 } }, target),
  descriptor('launch', 'Open a local application executable discovered with computer.apps/windows or supplied by the user. Requires an absolute .exe path; shell/interpreter commands are unsupported. Background Chrome/Edge adds the declared anti-occlusion rendering flag before approval. Do not replace sandboxed terminal execution.',
    { appPath: { type: 'string', minLength: 1, maxLength: 4096 }, args: { type: 'array', items: { type: 'string' }, maxItems: 32 },
      background: { type: 'boolean', default: true, description: 'Default true: best-effort launch behind the current foreground window without minimizing the target, so window screenshots remain available. Applications can override it; the receipt reports observed foreground state.' } }, ['appPath']),
  descriptor('window', 'Resize, maximize, minimize or restore the identified window without requesting foreground activation. Resize uses client physical pixels; observe actual returned dimensions/state.',
    { ...window, mode: { type: 'string', enum: ['resize', 'maximize', 'minimize', 'restore'] },
      width: { type: 'integer', minimum: 64, maximum: 8192 }, height: { type: 'integer', minimum: 64, maximum: 8192 } }, [...target, 'mode']),
  descriptor('activate', 'Bring the identified visible window to the foreground. Returns failure if Windows refuses activation.', window, target),
  descriptor('move', 'Move the mouse within the identified foreground window. Coordinates are physical pixels relative to its client origin.', { ...window, ...point }, [...target, 'x', 'y']),
  descriptor('click', 'Click within the identified foreground window at client pixel coordinates. Observe/read again after an action.',
    { ...window, ...point, button: { type: 'string', enum: ['left', 'right', 'middle'] } }, [...target, 'x', 'y']),
  descriptor('scroll', 'Scroll at a point in the identified foreground window. delta is bounded wheel units: positive upwards, negative downwards.',
    { ...window, ...point, delta: { type: 'integer', minimum: -1200, maximum: 1200 } }, [...target, 'x', 'y', 'delta']),
  descriptor('drag', 'Drag the left mouse button between client pixel coordinates inside the identified foreground window.',
    { ...window, ...point, endX: point.x, endY: point.y }, [...target, 'x', 'y', 'endX', 'endY']),
  descriptor('type', 'Type bounded Unicode text into the focused control of the identified foreground window. Does not use the clipboard.',
    { ...window, text: { type: 'string', minLength: 1, maxLength: 4000 } }, [...target, 'text']),
  descriptor('key', 'Send one supported key or key combination to the identified foreground window, such as ENTER, TAB, CTRL+A or SHIFT+TAB. No Windows/system shortcut automation.',
    { ...window, key: { type: 'string', enum: computerKeyNames } }, [...target, 'key'])
];
