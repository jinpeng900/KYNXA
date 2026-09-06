using System.Text.Json;
using Windows.Storage;
using KYNXA_Desktop.Models.UI;

namespace KYNXA_Desktop.Services;

public sealed class LayoutStateService
{
    private const string SettingsKey = "kynxa.ui.layout.v2";

    public LayoutState Load()
    {
        try
        {
            if (ApplicationData.Current.LocalSettings.Values[SettingsKey] is string json)
            {
                LayoutState? state = JsonSerializer.Deserialize<LayoutState>(json);
                if (state is not null && state.LayoutVersion <= LayoutState.CurrentVersion)
                {
                    state.LayoutVersion = LayoutState.CurrentVersion;
                    return state;
                }
            }
        }
        catch
        {
            // A damaged preference must never prevent the shell from starting.
        }

        return LayoutState.CreateDefault();
    }

    public void Save(LayoutState state)
    {
        try
        {
            ApplicationData.Current.LocalSettings.Values[SettingsKey] = JsonSerializer.Serialize(state);
        }
        catch
        {
            // Layout persistence is best-effort; the in-memory layout remains usable.
        }
    }
}
