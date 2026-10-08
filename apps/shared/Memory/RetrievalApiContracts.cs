using System.Text.Json;
using System.Text.Json.Serialization;

namespace KYNXA.Contracts;

/// <summary>
/// Retrieval configuration and derived-index transport; original conversations remain authoritative.
/// 检索配置与派生索引传输合同；原始聊天仍为正式数据来源。
/// </summary>
public sealed record RetrievalSettingsDocument(int SchemaVersion, long Revision, RetrievalLocalSettings Local,
    RetrievalWebSettings Web, RetrievalCacheSettings Cache);

public sealed record RetrievalLocalSettings(bool Enabled, string Semantic, string EmbeddingProfileId,
    string VectorBackend = "sqlite", string? RerankProfileId = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? EmbeddingDevicePolicy = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalIndexingLimits? Indexing = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalAnnSettings? Ann = null);

// Missing optional settings stay omitted, so an older settings screen cannot reset backend-owned policy.
// 可选设置未指定时保持省略，防止旧设置界面把后端设备策略与容量配置重置为默认值。
public sealed record RetrievalIndexingLimits(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaximumFiles = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? MaximumSourceBytes = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? MaximumTotalBytes = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaximumEntries = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? BatchSize = null);
public sealed record RetrievalAnnSettings(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Mode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? Adaptive = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? Threshold = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? MaxCachedShards = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? MaxShardBytes = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? Connectivity = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? ExpansionAdd = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? ExpansionSearch = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? ExactScanLimit = null);
public sealed record RetrievalWebSettings(string Mode, string ProviderId, string Depth, string Language,
    string BrowserRead = "auto");
public sealed record RetrievalCacheSettings(long MemoryLimitBytes, long DiskLimitBytes);

// Nullable patch fields are omitted, preserving settings outside this window's responsibility.
// 可空补丁字段不发送，保留本窗口职责以外的设置。
public sealed record RetrievalSettingsPatch(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalLocalSettings? Local = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalWebSettings? Web = null);
public sealed record RetrievalSettingsUpdateRequest(long ExpectedRevision, RetrievalSettingsPatch Patch);

public sealed record RetrievalSettingsOverrides(RetrievalLocalSettings? Local = null, RetrievalWebSettings? Web = null);
public sealed record RetrievalMountedFolderSettings(bool Enabled, long BindingRevision = 0);
public sealed record RetrievalIndexingSources(RetrievalMountedFolderSettings MountedFolder, string[] KnowledgeIds);
public sealed record RetrievalEffectiveProjectIndexing(bool MountedFolder, long BindingRevision, string[] KnowledgeIds);
public sealed record RetrievalEffectiveSettings(RetrievalLocalSettings Local, RetrievalWebSettings Web,
    RetrievalCacheSettings Cache, RetrievalEffectiveProjectIndexing? ProjectIndexing = null);
public sealed record ProjectRetrievalSettingsDocument(int SchemaVersion, Guid ProjectId, long Revision,
    RetrievalSettingsOverrides Overrides, RetrievalIndexingSources IndexingSources, RetrievalEffectiveSettings Effective);
public sealed record RetrievalMountedFolderPatch(bool Enabled);
public sealed record RetrievalIndexingSourcesPatch(RetrievalMountedFolderPatch MountedFolder);
public sealed record ProjectRetrievalSettingsPatch(RetrievalSettingsOverrides Overrides,
    RetrievalIndexingSourcesPatch IndexingSources);
public sealed record ProjectRetrievalSettingsUpdateRequest(long ExpectedRevision, ProjectRetrievalSettingsPatch Patch);

public sealed record RetrievalEmbeddingStatus(string State, string ProfileId, int Dimensions, bool Available,
    string? Message = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? Loaded = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ErrorCode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalInferenceBackendStatus? InferenceBackend = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? ResourceReservation = null);
public sealed record RetrievalInferenceBackendStatus(string Device,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? CpuThreads = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] int? DeviceId = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? Dtype = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? GpuValidated = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ExecutionMode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? CpuOperatorFallback = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Diagnostic = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? OperatorAudit = null);
public sealed record RetrievalRerankingStatus(string State,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ProfileId = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? Loaded = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ErrorCode = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalInferenceBackendStatus? InferenceBackend = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? ResourceReservation = null);

// Dynamic observations retain their bounded HTTP payload without coupling UI contracts to native allocator internals.
// 动态观察沿用有界 HTTP 响应，不把界面合同绑定到原生分配器内部布局；有 GPU 不代表正在用 GPU。
public sealed record RetrievalStatus(string Backend, int SourceCount, int ChunkCount,
    RetrievalEmbeddingStatus Embedding, RetrievalIndexJob[] Jobs,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Resources = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? VectorSpacePolicy = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Deployment = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? ExternalModels = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalRerankingStatus? Reranking = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? EmbeddingProfiles = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Parsing = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? ModelProfiles = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] string? ConfiguredEmbeddingProfileId = null);
public sealed record RetrievalProvider(string Id, string Name, bool Ready, string? Origin = null);
public sealed record RetrievalProvidersResponse(RetrievalProvider[] Providers, string SelectedId = "auto");
public sealed record RetrievalSource(string Id, string Title, string Scope, Guid? ProjectId, string Path,
    string Status, string? Error = null, long? Revision = null, int? ImportedCount = null, string? JobId = null);
public sealed record RetrievalSourcesResponse(RetrievalSource[] Sources);
public sealed record RetrievalSourceImportRequest(string Scope, string Path, Guid? ProjectId = null);
public sealed record RetrievalSourceDeleteRequest(
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] long? ExpectedRevision = null);
public sealed record RetrievalIndexRebuildRequest(Guid? ProjectId = null);
public sealed record RetrievalIndexJob(string JobId, string Status, int CompletedSources, int TotalSources,
    string? Error = null, DateTimeOffset? StartedAt = null, DateTimeOffset? FinishedAt = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] RetrievalCoverageStatus? Coverage = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Semantic = null);

// Status stays a string for forward compatibility, including partial; coverage describes actual eligible-source work.
// 状态保留字符串兼容未来值和 partial；覆盖统计只说明实际来源处理，不代表答案或任务正确性。
public sealed record RetrievalCoverageStatus(int Discovered, int Lexical, int Semantic, int Failed, int Skipped,
    int Partial, bool Complete,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] bool? ReportTruncated = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Sources = null,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.WhenWritingNull)] JsonElement? Failures = null);
