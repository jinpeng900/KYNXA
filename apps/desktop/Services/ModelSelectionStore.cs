using System.Text.Json;

namespace KYNXA_Desktop.Services;

/// <summary>The composer's UI selection only; does not store or imply connection credentials.</summary>
public sealed class ModelSelectionStore(string dataDirectory)
{
    private readonly string _path = Path.Combine(dataDirectory, "model-selection.json");

    public string? Load() => File.Exists(_path)
        ? JsonSerializer.Deserialize<string?>(File.ReadAllText(_path)) : null;

    public void Save(string? name)
    {
        Directory.CreateDirectory(dataDirectory);
        File.WriteAllText(_path + ".tmp", JsonSerializer.Serialize(name));
        File.Move(_path + ".tmp", _path, overwrite: true);
    }
}
