function option(args, names) {
  for (let index = 0; index < args.length; index++) {
    for (const name of names) {
      if (args[index] === name) return args[index + 1] ?? '';
      if (args[index].startsWith(name + '=')) return args[index].slice(name.length + 1);
    }
  }
  return null;
}

/**
 * Describe the configured boundary, never infer authentication or disclose endpoint credentials.
 * 只描述配置中声明的边界，不推断认证状态，也不暴露端点凭据。
 */
export function browserConnection(server) {
  if ((server.transport ?? 'stdio') !== 'stdio') return null;
  const args = server.args ?? [], executable = String(server.command ?? '').replaceAll('\\', '/').split('/').at(-1);
  const packageArgs = args.map(arg => String(arg).replaceAll('\\', '/')).join(' ');
  const engine = /(?:^|[\s/])@playwright\/mcp(?:@|[\s/]|$)|playwright-mcp/.test(packageArgs + ' ' + executable) ? 'playwright'
    : /(?:^|[\s/])chrome-devtools-mcp(?:@|[\s/]|\.(?:cmd|exe)(?:\s|$)|$)/.test(packageArgs + ' ' + executable) ? 'chrome-devtools' : null;
  if (!engine) return null;
  const endpoint = option(args, engine === 'playwright' ? ['--cdp-endpoint', '--endpoint']
    : ['--browserUrl', '--browser-url', '-u', '--wsEndpoint', '--ws-endpoint', '-w']);
  const attached = args.some(arg => /^(?:--autoConnect|--auto-connect|--extension)(?:=true)?$/.test(arg));
  let mode = attached ? 'existing-browser' : endpoint !== null ? 'remote-browser' : 'independent-browser';
  if (args.includes('--config') || args.some(arg => arg.startsWith('--config=')) ||
      (engine === 'playwright' && Object.keys({ ...server.env, ...server.envRefs }).some(name =>
        /^PLAYWRIGHT_MCP_(?:CDP_ENDPOINT|ENDPOINT|EXTENSION|CONFIG|HEADLESS|ISOLATED|USER_DATA_DIR)$/.test(name)))) mode = 'custom-browser';
  return { engine, mode, headless: args.some(arg => /^(?:--headless)(?:=true)?$/.test(arg)),
    isolated: args.some(arg => /^(?:--isolated)(?:=true)?$/.test(arg)) };
}

export function browserConnectionPrompt(servers) {
  const descriptions = {
    'existing-browser': 'Connects to a local existing browser and its existing signed-in pages; initial browser connection approval may be required. This does not extract cookies.',
    'remote-browser': 'Connects to an explicitly configured CDP/Playwright endpoint. It may be a remote browser or a local debugging endpoint; verify the page identity. It does not inherit the user\'s local browser session.',
    'independent-browser': 'Starts a separate local browser profile, not the user\'s everyday browser. Never label it a cloud browser or claim it is signed in without page evidence.',
    'custom-browser': 'Uses custom browser arguments/environment. Determine its actual connection/session before claiming visibility or sign-in.'
  };
  return servers.filter(server => server.enabled).flatMap(server => {
    const connection = browserConnection(server);
    if (!connection) return [];
    return [`Browser MCP ${server.id}: ${descriptions[connection.mode]}${connection.mode === 'independent-browser'
      ? ` Window is ${connection.headless ? 'headless (not visible)' : 'visible'}.` : ''}`];
  });
}
