using System.Text.Json;

namespace KYNXA_Desktop.Services;

public sealed record ModelChoice(string ProviderId, string ProviderName, string ModelId)
{
    public string Name => ModelCatalog.Describe(ModelId).Name;
    public string Label => $"{Name} · {ProviderName}";
    public override string ToString() => Label;
}

/// <summary>
/// The composer's UI selection only; does not store connection credentials.
/// 只保存输入区的 UI 选择，不保存模型连接凭据。
/// </summary>
public sealed class ModelSelectionStore(string dataDirectory)
{
    private readonly string _path = Path.Combine(dataDirectory, "model-selection.json");

    public ModelChoice? Load()
    {
        if (!File.Exists(_path)) return null;
        try { return JsonSerializer.Deserialize<ModelChoice>(File.ReadAllText(_path)); }
        catch (JsonException) { return null; } // Ignore the old name-only preference.
        // 忽略旧版仅按模型名称保存的偏好。
    }

    public void Save(ModelChoice? choice)
    {
        Directory.CreateDirectory(dataDirectory);
        File.WriteAllText(_path + ".tmp", JsonSerializer.Serialize(choice));
        File.Move(_path + ".tmp", _path, overwrite: true);
    }
}
