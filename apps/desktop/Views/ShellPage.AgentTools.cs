using KYNXA_Desktop.Services;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private readonly AgentApiClient _agentApiClient = new();
    private ToolManagementWindow? _toolManagementWindow;

    private void OpenAgentTools()
    {
        if (!_projectsReady || StoragePaths.IsMigrating || _projectActionPending || _sendingPrompt) return;
        if (_toolManagementWindow is { } existing)
        {
            if (existing.AppWindow.Presenter is OverlappedPresenter presenter) presenter.Restore();
            existing.Activate();
            return;
        }
        var active = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        var window = new ToolManagementWindow(conversationId: active is { CanPersist: true } ? active.Id : null);
        _toolManagementWindow = window;
        var owner = App.Window.AppWindow;
        window.AppWindow.Move(new Windows.Graphics.PointInt32(owner.Position.X + Math.Max(0, (owner.Size.Width - 940) / 2),
            owner.Position.Y + Math.Max(0, (owner.Size.Height - 720) / 2)));
        void CloseTools(object sender, WindowEventArgs e) => window.CloseForOwner();
        App.Window.Closed += CloseTools;
        window.Closed += (_, _) =>
        {
            App.Window.Closed -= CloseTools;
            _toolManagementWindow = null;
        };
        window.Activate();
    }
}
