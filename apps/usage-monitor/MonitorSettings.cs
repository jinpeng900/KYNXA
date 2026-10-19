using System.IO;
using System.Text.Json;

namespace Kynxa.UsageMonitor;

public sealed class MonitorSettings
{
    public int SchemaVersion { get; set; } = 1;
    public double? Left { get; set; }
    public double? Top { get; set; }
    public bool Topmost { get; set; } = true;
    public bool Collapsed { get; set; }

    public static MonitorSettings Load(string directory)
    {
        try
        {
            string path = Path.Combine(directory, "placement.json");
            if (!File.Exists(path)) return new();
            if (new FileInfo(path).Length > 16_384) throw new IOException("settings_too_large");
            MonitorSettings settings = JsonSerializer.Deserialize<MonitorSettings>(File.ReadAllText(path)) ?? new();
            if (settings.SchemaVersion != 1) throw new IOException("unsupported_settings");
            if (settings.Left is double left && !double.IsFinite(left)) settings.Left = null;
            if (settings.Top is double top && !double.IsFinite(top)) settings.Top = null;
            return settings;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or JsonException)
        {
            // Invalid optional placement settings must not block the owner's main interface.
            // 可选位置设置损坏不能阻止宿主主界面启动。
            return new();
        }
    }

    public void Save(string directory)
    {
        Directory.CreateDirectory(directory);
        string path = Path.Combine(directory, "placement.json");
        string temporaryPath = path + ".tmp";
        File.WriteAllText(temporaryPath, JsonSerializer.Serialize(this, new JsonSerializerOptions { WriteIndented = true }));
        File.Move(temporaryPath, path, true);
    }
}
