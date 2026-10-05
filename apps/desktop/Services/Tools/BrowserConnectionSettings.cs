namespace KYNXA_Desktop.Services;

public enum BrowserConnectionMode { Independent, Existing, Remote, Custom }

public sealed record BrowserConnectionSettings(string Engine, BrowserConnectionMode Mode, bool ShowWindow, string Endpoint,
    bool HasEnvironmentConfiguration = false)
{
    private static readonly HashSet<string> ValueOptions = new(StringComparer.Ordinal)
    {
        "--cdp-endpoint", "--endpoint", "--browserUrl", "--browser-url", "-u", "--wsEndpoint", "--ws-endpoint", "-w",
        "--user-data-dir", "--userDataDir", "--profile-dir-name"
    };
    private static readonly HashSet<string> SwitchOptions = new(StringComparer.Ordinal)
    { "--headless", "--isolated", "--extension", "--autoConnect", "--auto-connect" };

    public static BrowserConnectionSettings? Read(string command, IReadOnlyList<string> args, IEnumerable<string>? environmentNames = null)
    {
        // The editor can temporarily contain valid JSON with invalid members while the user types.
        // Leave its browser controls unavailable until Save performs the formal input validation.
        // 输入期间 JSON 可能合法而成员值暂时无效；正式保存验证之前不启用浏览器编辑控件。
        if (args.Any(argument => argument is null)) return null;
        string source = string.Join(' ', args.Append(command.Replace('\\', '/')));
        string? engine = source.Contains("@playwright/mcp", StringComparison.Ordinal) || source.Contains("playwright-mcp", StringComparison.Ordinal)
            ? "playwright" : source.Contains("chrome-devtools-mcp", StringComparison.Ordinal) ? "chrome-devtools" : null;
        if (engine is null) return null;
        bool environment = engine == "playwright" && (environmentNames ?? []).Any(name =>
            name is "PLAYWRIGHT_MCP_CDP_ENDPOINT" or "PLAYWRIGHT_MCP_ENDPOINT" or "PLAYWRIGHT_MCP_EXTENSION" or "PLAYWRIGHT_MCP_CONFIG" or
                "PLAYWRIGHT_MCP_HEADLESS" or "PLAYWRIGHT_MCP_ISOLATED" or "PLAYWRIGHT_MCP_USER_DATA_DIR");
        string endpoint = "";
        bool existing = false, show = true, custom = false;
        for (int index = 0; index < args.Count; index++)
        {
            string argument = args[index], name = argument.Split('=', 2)[0];
            if (name == "--config") custom = true;
            if (SwitchOptions.Contains(name))
            {
                bool enabled = ReadSwitch(args, ref index);
                if (name == "--headless") show = !enabled;
                if (name is "--autoConnect" or "--auto-connect" or "--extension") existing |= enabled;
                continue;
            }
            if (!ValueOptions.Contains(name) || name is "--user-data-dir" or "--userDataDir" or "--profile-dir-name") continue;
            endpoint = argument.Contains('=') ? argument[(argument.IndexOf('=') + 1)..] : index + 1 < args.Count ? args[++index] : "";
        }
        return new(engine, custom || environment ? BrowserConnectionMode.Custom : existing ? BrowserConnectionMode.Existing :
            endpoint.Length > 0 ? BrowserConnectionMode.Remote : BrowserConnectionMode.Independent, show, endpoint, environment);
    }

    public string[] Apply(IReadOnlyList<string> previous)
    {
        if (Mode == BrowserConnectionMode.Custom) return previous.ToArray();
        if (HasEnvironmentConfiguration || previous.Any(arg => arg == "--config" || arg.StartsWith("--config=", StringComparison.Ordinal)))
            throw new ArgumentException("浏览器使用配置文件，请在高级设置中修改连接方式。");
        var args = new List<string>();
        // Direct executable configurations may carry no package name in their arguments.
        // Retain their profile when the engine's existing mode remains unchanged.
        // 直接启动程序的配置可能没有包名参数；浏览器模式未变时保留原用户配置目录。
        var previousMode = Read(Engine == "playwright" ? "playwright-mcp" : "chrome-devtools-mcp", previous)?.Mode;
        for (int index = 0; index < previous.Count; index++)
        {
            string argument = previous[index], name = argument.Split('=', 2)[0];
            if (ValueOptions.Contains(name))
            {
                // Preserve explicitly configured independent profile paths in the same mode.
                // 同一模式下保留用户明确配置的独立浏览器目录。
                if ((Mode == BrowserConnectionMode.Independent && (name is "--user-data-dir" or "--userDataDir")) ||
                    (name == "--profile-dir-name" && previousMode == Mode))
                {
                    args.Add(argument);
                    if (!argument.Contains('=') && index + 1 < previous.Count) args.Add(previous[++index]);
                }
                else if (!argument.Contains('=') && index + 1 < previous.Count) index++;
                continue;
            }
            if (!SwitchOptions.Contains(name)) args.Add(argument);
            else if (!argument.Contains('=') && index + 1 < previous.Count && previous[index + 1] is "true" or "false") index++;
        }
        switch (Mode)
        {
            case BrowserConnectionMode.Independent:
                if (!ShowWindow) args.Add("--headless");
                if (!args.Any(arg => arg.Split('=', 2)[0] is "--user-data-dir" or "--userDataDir")) args.Add("--isolated");
                break;
            case BrowserConnectionMode.Existing:
                args.Add(Engine == "playwright" ? "--extension" : "--autoConnect");
                break;
            case BrowserConnectionMode.Remote:
                if (!Uri.TryCreate(Endpoint.Trim(), UriKind.Absolute, out var uri) || uri.UserInfo.Length > 0 || uri.Fragment.Length > 0 ||
                    uri.Scheme is not ("http" or "https" or "ws" or "wss"))
                    throw new ArgumentException("请输入有效的浏览器连接地址（HTTP 或 WebSocket）。");
                bool playwrightEndpoint = Engine == "playwright" && previous.Any(argument => argument == "--endpoint" ||
                    argument.StartsWith("--endpoint=", StringComparison.Ordinal));
                args.Add(Engine == "playwright" ? playwrightEndpoint ? "--endpoint" : "--cdp-endpoint" :
                    uri.Scheme is "ws" or "wss" ? "--wsEndpoint" : "--browserUrl");
                args.Add(Endpoint.Trim());
                break;
        }
        return args.ToArray();
    }

    private static bool ReadSwitch(IReadOnlyList<string> args, ref int index)
    {
        string argument = args[index];
        if (argument.Contains('=')) return !argument.EndsWith("=false", StringComparison.Ordinal);
        if (index + 1 < args.Count && args[index + 1] is "true" or "false") return args[++index] == "true";
        return true;
    }
}
