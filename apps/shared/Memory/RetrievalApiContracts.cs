using System.Text.Json.Serialization;

namespace KYNXA.Contracts;

/// <summary>
/// Retrieval configuration and derived-index transport; original conversations remain authoritative.
/// 检索配置与派生索引传输合同；原始聊天仍为正式数据来源。
/// </summary>
public sealed record RetrievalSettingsDocument(int SchemaVersion, long Revision, RetrievalLocalSettings Local,
    RetrievalWebSettings Web, RetrievalCacheSettings Cache);

public sealed record RetrievalLocalSettings(bool Enabled, string Semantic, string EmbeddingProfileId,
    string VectorBackend = "sqlite", string? RerankProfileId = null);
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
    string? Message = null);
public sealed record RetrievalStatus(string Backend, int SourceCount, int ChunkCount,
    RetrievalEmbeddingStatus Embedding, RetrievalIndexJob[] Jobs);
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
    string? Error = null, DateTimeOffset? StartedAt = null, DateTimeOffset? FinishedAt = null);
