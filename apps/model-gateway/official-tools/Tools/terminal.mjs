export const hostTerminalDescriptor = {
  name: 'terminal.host.run', source: 'builtin',
  description: 'Run local CMD/PowerShell with the app host PATH, outside the sandbox; Ask/Smart approval required. Shell profiles are not loaded: resolve Conda or initialize the selected environment in this same script. Captured output is returned. Maximum 120 seconds; use terminal.host.start/read/stop for controlled background work. Use visible:true only for a requested separate window. Interrupted effects need verification, never replay.',
  inputSchema: { type: 'object', additionalProperties: false,
    properties: { shell: { type: 'string', enum: ['cmd', 'powershell'] },
      script: { type: 'string', minLength: 1, maxLength: 16384 },
      cwd: { type: 'string', minLength: 1, maxLength: 4096 },
      timeoutMs: { type: 'integer', minimum: 100, maximum: 120000 },
      visible: { type: 'boolean', description: 'Open a real terminal window. Default false (captured background command).' },
      keepOpenMs: { type: 'integer', minimum: 0, maximum: 30000,
        description: 'Visible only: hold the window after the command. Default 5000 ms, within timeoutMs.' },
      reason: { type: 'string', minLength: 1, maxLength: 2000 } },
    required: ['shell', 'script', 'reason'] }
};

export const hostTerminalJobDescriptors = [
  { name: 'terminal.host.start', source: 'builtin',
    description: 'Start a controlled hidden host CMD/PowerShell job in this chat. Returns a verified process start, not task completion. Default 30 minutes, maximum 6 hours; continues after this model response. Use terminal.host.read for status/output and terminal.host.stop to terminate its process tree. Gateway shutdown cancels jobs; restart never replays commands. Reason and Ask/Smart approval required.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      shell: { type: 'string', enum: ['cmd', 'powershell'] }, script: { type: 'string', minLength: 1, maxLength: 16384 },
      cwd: { type: 'string', minLength: 1, maxLength: 4096 }, timeoutMs: { type: 'integer', minimum: 100, maximum: 21600000 },
      reason: { type: 'string', minLength: 1, maxLength: 2000 }
    }, required: ['shell', 'script', 'reason'] } },
  { name: 'terminal.host.read', source: 'builtin',
    description: 'Read status, final execution receipt and a bounded output page of a host job started in this chat. Does not run or restart commands. Retained output has explicit truncation and paging offsets.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      jobId: { type: 'string', minLength: 1, maxLength: 40 }, offset: { type: 'integer', minimum: 0, maximum: 9007199254740991 },
      limit: { type: 'integer', minimum: 1, maximum: 64000 }
    }, required: ['jobId'] } },
  { name: 'terminal.host.stop', source: 'builtin',
    description: 'Cancel a host job belonging to this chat and wait for its process tree cleanup and actual receipt. Does not replay the command; Ask/Smart approval required.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {
      jobId: { type: 'string', minLength: 1, maxLength: 40 }, reason: { type: 'string', minLength: 1, maxLength: 2000 }
    }, required: ['jobId', 'reason'] } }
];
