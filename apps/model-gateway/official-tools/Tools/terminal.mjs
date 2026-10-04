export const hostTerminalDescriptor = {
  name: 'terminal.host.run', source: 'builtin',
  description: 'Run local CMD/PowerShell with host PATH (e.g. conda), outside the sandbox; Ask/Smart approval required. Captured output is returned in the tool result. Use visible:true only for a requested separate window: real console input/output, bounded screen preview, brief hold after completion. No hidden fallback. Nonzero exit is finished; interrupted effects need verification, never replay.',
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
