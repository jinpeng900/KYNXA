using System.Net;
using KYNXA.Contracts;
using KYNXA_Desktop.Services;

namespace RetrievalUiSmoke;

internal sealed class FakeRetrievalApi : IRetrievalApi
{
    public Guid ProjectId { get; } = Guid.NewGuid();
    public RetrievalSettingsDocument Global { get; private set; } = new(1, 4, new(true, "auto", "builtin-multilingual"),
        new("auto", "auto", "standard", "auto"), new(67108864, 536870912));
    public ProjectRetrievalSettingsDocument Project { get; private set; }
    public RetrievalIndexJob Job { get; set; } = new("synthetic_job", "running", 1, 2);
    public int GlobalWrites { get; private set; }
    public int ProjectWrites { get; private set; }
    public int Rebuilds { get; private set; }
    public int CancelledJobs { get; private set; }
    public int JobReads { get; private set; }
    public string? LastCancelledJobId { get; private set; }
    public RetrievalIndexJob[] StatusJobs { get; set; } = [];
    public RetrievalSource[] Sources { get; set; } = [];
    public bool ConflictNextSave { get; set; }
    public TaskCompletionSource? SaveGate { get; set; }

    public FakeRetrievalApi()
    {
        Project = new(1, ProjectId, 2, new(), new(new(false), []), new(Global.Local, Global.Web, Global.Cache, new(false, 0, [])));
    }

    public Task<RetrievalSettingsDocument> GetSettingsAsync(CancellationToken cancellationToken = default) => Task.FromResult(Global);
    public async Task<RetrievalSettingsDocument> SaveSettingsAsync(RetrievalSettingsUpdateRequest request, CancellationToken cancellationToken = default)
    {
        GlobalWrites++;
        if (SaveGate is { } gate) await gate.Task.WaitAsync(cancellationToken);
        if (ConflictNextSave)
        {
            ConflictNextSave = false;
            Global = Global with { Revision = Global.Revision + 1 };
            throw new GatewayApiException("Synthetic conflict", HttpStatusCode.Conflict, "RETRIEVAL_REVISION_CONFLICT");
        }
        if (request.ExpectedRevision != Global.Revision) throw new GatewayApiException("Synthetic conflict", HttpStatusCode.Conflict);
        Global = Global with { Revision = Global.Revision + 1, Local = request.Patch.Local ?? Global.Local, Web = request.Patch.Web ?? Global.Web };
        return Global;
    }
    public Task<ProjectRetrievalSettingsDocument> GetProjectSettingsAsync(Guid projectId, CancellationToken cancellationToken = default) => Task.FromResult(Project);
    public Task<ProjectRetrievalSettingsDocument> SaveProjectSettingsAsync(Guid projectId, ProjectRetrievalSettingsUpdateRequest request,
        CancellationToken cancellationToken = default)
    {
        ProjectWrites++;
        if (request.ExpectedRevision != Project.Revision) throw new GatewayApiException("Synthetic conflict", HttpStatusCode.Conflict);
        var mounted = new RetrievalMountedFolderSettings(request.Patch.IndexingSources.MountedFolder.Enabled, Project.IndexingSources.MountedFolder.BindingRevision + 1);
        Project = Project with { Revision = Project.Revision + 1, Overrides = request.Patch.Overrides,
            IndexingSources = Project.IndexingSources with { MountedFolder = mounted },
            Effective = new(request.Patch.Overrides.Local ?? Global.Local, request.Patch.Overrides.Web ?? Global.Web, Global.Cache,
                new(mounted.Enabled, mounted.BindingRevision, [])) };
        return Task.FromResult(Project);
    }
    public Task<RetrievalStatus> GetStatusAsync(CancellationToken cancellationToken = default) => Task.FromResult(new RetrievalStatus("sqlite", 2, 8,
        new("ready", "builtin-multilingual", 384, true), StatusJobs));
    public Task<RetrievalProvidersResponse> GetProvidersAsync(CancellationToken cancellationToken = default) =>
        Task.FromResult(new RetrievalProvidersResponse([new("synthetic_search", "Synthetic search", true)]));
    public Task<RetrievalSourcesResponse> GetSourcesAsync(Guid? projectId = null, CancellationToken cancellationToken = default) =>
        Task.FromResult(new RetrievalSourcesResponse(Sources.Where(source => source.Scope == "user" || source.ProjectId == projectId).ToArray()));
    public Task<RetrievalSource> ImportSourceAsync(RetrievalSourceImportRequest request, CancellationToken cancellationToken = default) =>
        throw new NotSupportedException("Native picker import is tested separately from fake transport.");
    public Task DeleteSourceAsync(string sourceId, long? expectedRevision = null, CancellationToken cancellationToken = default) => Task.CompletedTask;
    public Task<RetrievalIndexJob> RebuildIndexAsync(Guid? projectId = null, CancellationToken cancellationToken = default)
    { Rebuilds++; return Task.FromResult(Job); }
    public Task<RetrievalIndexJob> GetIndexJobAsync(string jobId, CancellationToken cancellationToken = default)
    { JobReads++; return Task.FromResult(Job); }
    public Task<RetrievalIndexJob> CancelIndexJobAsync(string jobId, CancellationToken cancellationToken = default)
    { CancelledJobs++; LastCancelledJobId = jobId; Job = Job with { Status = "cancelled" }; return Task.FromResult(Job); }
}
