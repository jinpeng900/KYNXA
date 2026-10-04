using System.Globalization;
using System.Text.Json;
using KYNXA.Contracts;

namespace KYNXA_Desktop.Services;

/// <summary>
/// Human-readable desktop operation fields. This never authorizes or executes an operation.
/// 把桌面操作字段转换为可读描述；此处既不授权也不执行操作。
/// </summary>
public static class ComputerToolPresentation
{
    public sealed record Field(string LabelKey, string Value, bool LocalizedValue = false);
    private static readonly IReadOnlyDictionary<string, string> Actions = new Dictionary<string, string>
    {
        ["computer.windows"] = "查看窗口", ["computer.apps"] = "查找软件", ["computer.screenshot"] = "截图", ["computer.read"] = "读取窗口",
        ["computer.launch"] = "打开软件", ["computer.window"] = "调整窗口", ["computer.activate"] = "切换窗口", ["computer.click"] = "点击",
        ["computer.move"] = "移动鼠标", ["computer.scroll"] = "滚动", ["computer.drag"] = "拖动",
        ["computer.type"] = "输入文字", ["computer.key"] = "按下按键"
    };

    public static bool Supports(string name) => Actions.ContainsKey(name);
    public static string ActionKey(string name) => Actions.TryGetValue(name, out string? key) ? key : "执行操作";

    public static IReadOnlyList<Field> Fields(ToolActivity tool)
    {
        var fields = new List<Field>();
        if (tool.Arguments is not { ValueKind: JsonValueKind.Object } args) return fields;
        string Text(string key) => args.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.String
            ? Clean(value.GetString() ?? string.Empty) : string.Empty;
        string Number(string key) => args.TryGetProperty(key, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out long number)
            ? number.ToString(CultureInfo.InvariantCulture) : string.Empty;
        string Point(string x, string y) => $"({Number(x)}, {Number(y)})";
        if (tool.Name == "computer.launch")
        {
            fields.Add(new("程序", Text("appPath")));
            if (args.TryGetProperty("args", out var values) && values.ValueKind == JsonValueKind.Array)
            {
                string arguments = string.Join(" ", values.EnumerateArray().Where(value => value.ValueKind == JsonValueKind.String)
                    .Select(value => Quote(Clean(value.GetString() ?? string.Empty))));
                if (arguments.Length > 0) fields.Add(new("启动参数", arguments));
            }
            if (args.TryGetProperty("background", out var background) && background.ValueKind == JsonValueKind.True)
                fields.Add(new("后台启动", "是", true));
        }
        else if (tool.Name is not ("computer.windows" or "computer.apps"))
        {
            string id = Text("windowId"), pid = Number("processId");
            fields.Add(!string.IsNullOrEmpty(id) && !string.IsNullOrEmpty(pid)
                ? new("目标窗口", $"HWND {id} · PID {pid}") : new("目标窗口", "范围未提供", true));
        }
        if (tool.Name is "computer.click" or "computer.move" or "computer.scroll") fields.Add(new("位置", Point("x", "y")));
        if (tool.Name == "computer.drag") fields.Add(new("位置", Point("x", "y") + " → " + Point("endX", "endY")));
        if (tool.Name == "computer.scroll") fields.Add(new("滚动量", Number("delta")));
        if (tool.Name == "computer.click") fields.Add(new("鼠标按键", Text("button") switch { "right" => "右键", "middle" => "中键", _ => "左键" }, true));
        if (tool.Name == "computer.type")
        {
            int length = args.TryGetProperty("text", out var text) && text.ValueKind == JsonValueKind.String ? (text.GetString()?.Length ?? 0) : 0;
            fields.Add(new("输入字符数", length.ToString(CultureInfo.InvariantCulture)));
        }
        if (tool.Name == "computer.key") fields.Add(new("按键", Text("key")));
        if (tool.Name == "computer.window")
        {
            string mode = Text("mode") switch { "resize" => "调整大小", "maximize" => "最大化", "minimize" => "最小化",
                "restore" => "恢复窗口", _ => "范围未提供" };
            fields.Add(new("窗口模式", mode, true));
            if (Text("mode") == "resize") fields.Add(new("窗口尺寸", $"{Number("width")} × {Number("height")} px"));
        }
        if (tool.Name == "computer.screenshot" && args.TryGetProperty("crop", out var crop) && crop.ValueKind == JsonValueKind.Object &&
            crop.TryGetProperty("x", out var x) && x.TryGetInt32(out int left) &&
            crop.TryGetProperty("y", out var y) && y.TryGetInt32(out int top) &&
            crop.TryGetProperty("width", out var width) && width.TryGetInt32(out int captureWidth) &&
            crop.TryGetProperty("height", out var height) && height.TryGetInt32(out int captureHeight))
            fields.Add(new("截图区域", $"({left}, {top}) · {captureWidth} × {captureHeight} px"));
        return fields;
    }

    public static string Reason(ToolActivity tool)
    {
        if (tool.Arguments is not { ValueKind: JsonValueKind.Object } args || !args.TryGetProperty("reason", out var reason) ||
            reason.ValueKind != JsonValueKind.String) return string.Empty;
        string text = reason.GetString() ?? string.Empty;
        if (tool.Name == "computer.type" && args.TryGetProperty("text", out var input) && input.ValueKind == JsonValueKind.String &&
            input.GetString() is { Length: > 0 } original) text = text.Replace(original, "…", StringComparison.Ordinal);
        return Clean(text);
    }

    private static string Clean(string text) => new(text.Where(character => !char.IsControl(character) || character is '\n' or '\t').ToArray());
    private static string Quote(string text) => text.Length == 0 || text.Any(char.IsWhiteSpace) || text.Contains('"')
        ? "\"" + text.Replace("\"", "\\\"", StringComparison.Ordinal) + "\"" : text;
}
