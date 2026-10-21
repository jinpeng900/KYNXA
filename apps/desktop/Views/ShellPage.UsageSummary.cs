using System.Net.Http;
using System.Text.Json;
using KYNXA_Desktop.Services;
using Microsoft.UI.Xaml;

namespace KYNXA_Desktop.Views;

public sealed partial class ShellPage
{
    private sealed class UsageSummaryScope(Guid conversationId) : IDisposable
    {
        public Guid ConversationId { get; } = conversationId;
        public CancellationTokenSource Cancellation { get; } = new();
        public Task? RefreshTask { get; set; }
        public bool RefreshRequested { get; set; }
        public bool ReplyActive { get; set; }
        public void Dispose() { Cancellation.Cancel(); Cancellation.Dispose(); }
    }

    private readonly UsageApiClient _usageApiClient = new();
    private UsageSummaryScope? _usageSummaryScope;
    private bool _usageSummaryLoaded;
    private double UsageSummaryHeight => UsageSummary.Visibility == Visibility.Visible ? UsageSummary.ActualHeight + 12 : 0;

    private void InitializeUsageSummary()
    {
        Loaded += (_, _) => { _usageSummaryLoaded = true; RefreshUsageSummaryScope(); };
        Unloaded += (_, _) =>
        {
            _usageSummaryLoaded = false;
            _usageSummaryScope?.Dispose(); _usageSummaryScope = null;
        };
        UsageSummary.DetailsChanged += (_, _) =>
        {
            if (UsageSummary.IsDetailsExpanded) RequestUsageSummaryRefresh();
            ApplyLayout();
        };
        UsageSummary.SizeChanged += (_, _) => { if (_usageSummaryLoaded && !_chatClosing) ApplyLayout(); };
    }

    private void RefreshUsageSummaryScope()
    {
        if (UsageSummary is null) return;
        var chat = ViewModel.IsChatMode ? _activeStandaloneChat : _activeProjectChat;
        if (!_usageSummaryLoaded || _chatClosing || chat is not { IsPersisted: true })
        {
            _usageSummaryScope?.Dispose(); _usageSummaryScope = null;
            UsageSummary.Visibility = Visibility.Collapsed;
            return;
        }
        bool replying = IsReplyInProgress(chat.Id);
        if (_usageSummaryScope is { } current && current.ConversationId == chat.Id)
        {
            bool settled = current.ReplyActive && !replying;
            current.ReplyActive = replying;
            if (settled) RequestUsageSummaryRefresh();
            return;
        }
        _usageSummaryScope?.Dispose();
        _usageSummaryScope = new(chat.Id) { ReplyActive = replying };
        UsageSummary.Reset(); UsageSummary.Visibility = Visibility.Visible;
        RequestUsageSummaryRefresh();
    }

    private void RequestUsageSummaryRefresh()
    {
        if (_usageSummaryScope is not { } scope || !_usageSummaryLoaded || _chatClosing) return;
        scope.RefreshRequested = true;
        if (scope.RefreshTask is null || scope.RefreshTask.IsCompleted)
            scope.RefreshTask = RefreshUsageSummaryAsync(scope, scope.Cancellation.Token);
    }

    private async Task RefreshUsageSummaryAsync(UsageSummaryScope scope, CancellationToken cancellationToken)
    {
        // Compact rows refresh when the reply settles. Only expanded details poll, with one request in flight.
        // 紧凑行在回复结算时刷新；仅展开详情低频轮询，并且同一范围最多一个请求，不占发送前置。
        while (!cancellationToken.IsCancellationRequested && _usageSummaryScope == scope && _usageSummaryLoaded && !_chatClosing)
        {
            scope.RefreshRequested = false;
            try
            {
                var snapshot = await _usageApiClient.ReadConversationAsync(scope.ConversationId, cancellationToken);
                if (cancellationToken.IsCancellationRequested || _usageSummaryScope != scope || !_usageSummaryLoaded || _chatClosing) return;
                UsageSummary.Show(snapshot);
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { return; }
            catch (Exception error) when (error is HttpRequestException or IOException or OperationCanceledException or JsonException or InvalidOperationException)
            {
                if (cancellationToken.IsCancellationRequested || _usageSummaryScope != scope || !_usageSummaryLoaded || _chatClosing) return;
                UsageSummary.ShowUnavailable();
                System.Diagnostics.Debug.WriteLine($"Conversation usage: {error.GetType().Name}");
            }
            if (scope.RefreshRequested) continue;
            if (!UsageSummary.IsDetailsExpanded) return;
            try { await Task.Delay(TimeSpan.FromSeconds(5), cancellationToken); }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested) { return; }
            if (!UsageSummary.IsDetailsExpanded && !scope.RefreshRequested) return;
        }
    }
}
