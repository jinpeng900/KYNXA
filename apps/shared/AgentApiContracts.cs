using System.Text.Json;

namespace KYNXA.Contracts;

/// <summary>One public model round. Tool receipts belong to its round; private provider reasoning is never included.</summary>
public sealed record AssistantSegment(string Id, int Round, int Order, string Phase, string Status,
    string Content, string Reasoning, long ReasoningDurationMs = 0);

public static class AssistantSegmentRules
{
    public static bool IsValid(AssistantSegment? segment) => segment is not null &&
        !string.IsNullOrWhiteSpace(segment.Id) && segment.Id.Length <= 128 && segment.Id.All(character => char.IsAsciiLetterOrDigit(character) || character is '-' or '_') &&
        segment.Round is >= 1 and <= 128 && segment.Order is >= 0 and <= 1024 &&
        segment.Phase is "commentary" or "final_answer" && segment.Status is "streaming" or "completed" or "interrupted" &&
        segment.Content is not null && segment.Reasoning is not null &&
        (long)segment.Content.Length + segment.Reasoning.Length <= 2 * 1024 * 1024 && segment.ReasoningDurationMs >= 0;

    public static bool IsValidSequence(IReadOnlyList<AssistantSegment> segments)
    {
        if (segments.Count > 128) return false;
        var ids = new HashSet<string>(StringComparer.Ordinal);
        int lastRound = 0, lastOrder = -1;
        bool final = false;
        foreach (var segment in segments)
        {
            if (!IsValid(segment) || !ids.Add(segment.Id) || segment.Round <= lastRound || segment.Order <= lastOrder || final) return false;
            lastRound = segment.Round; lastOrder = segment.Order;
            final = segment.Phase == "final_answer";
        }
        return true;
    }
}

public sealed record ToolActivity(string ToolCallId, string Name, JsonElement? Arguments, string Status,
    string Summary, string? Result = null, Guid? ApprovalId = null, bool? OutsideWorkspace = null,
    string? Sandbox = null, string? WorkspaceRoot = null, ToolResultReference? ResultRef = null, string? Code = null,
    int? Round = null, int? Order = null);

/// <summary>Ephemeral terminal display data. Formal output remains in the tool receipt archive.</summary>
public sealed record HostTerminalOutput(string ToolCallId, int Sequence, string Stream, string Text, bool Replace = false);

public static class HostTerminalOutputRules
{
    public static bool IsValid(HostTerminalOutput? output) => output is not null &&
        !string.IsNullOrWhiteSpace(output.ToolCallId) && output.ToolCallId.Length <= 200 && output.Sequence >= 1 &&
        output.Text is not null && output.Text.Length <= 65536 &&
        (output.Stream is "stdout" or "stderr" && !output.Replace || output.Stream == "console" && output.Replace);
}

public sealed record ToolResultReference(Guid Id, long Bytes, string Sha256);
public sealed record ToolResultResponse(JsonElement Result);
public sealed record ToolResultPage(Guid Id, string Text, int TotalCharacters, int Offset, int NextOffset,
    bool Truncated, ToolResultReference ResultRef);

public sealed record McpServerConfig(string Id, string Name, string Command, string[] Args, bool Enabled,
    string? ProtocolVersion = null, string[]? DisabledTools = null, string Transport = "stdio", string? Cwd = null,
    Dictionary<string, string>? Env = null, Dictionary<string, string>? EnvRefs = null, string? Url = null,
    Dictionary<string, string>? HeaderEnv = null, McpAuthentication? Auth = null, int? StartupTimeoutMs = null,
    string? Origin = null, string? PresetId = null, bool? Overridden = null);
public sealed record McpAuthentication(string Type, string? TokenEnv = null, string? ClientId = null,
    string? ClientSecretEnv = null, string? Issuer = null, string? Scope = null);
public sealed record AgentConfig(int Version, long Revision, McpServerConfig[] McpServers, string[] SkillDirectories,
    string[]? DisabledSkills = null, string[]? DisabledOfficialMcpServers = null,
    string? OfficialToolsRoot = null, string? UserToolsRoot = null, string? OfficialPackageVersion = null);
public sealed record AgentConfigSaveRequest(int Version, long ExpectedRevision, McpServerConfig[] McpServers,
    string[] SkillDirectories, string[]? DisabledSkills = null, string[]? DisabledOfficialMcpServers = null);
public sealed record AgentSkillDiagnostic(string Code, string Message, string? Severity = null, string? Field = null);
public sealed record AgentSkillConflict(string PreferredId, string[] Ids, bool Preferred);
public sealed record AgentSkill(string Id, string Name, string Description, string Source, bool Enabled = true,
    bool? StandardCompliant = null, AgentSkillDiagnostic[]? Diagnostics = null, string? Status = null,
    string? Origin = null, int? Priority = null, AgentSkillConflict? Conflict = null);
public sealed record AgentSkillDetail(string Id, string Name, string Description, string Source, string Content);
public sealed record AgentSkillResponse(AgentSkillDetail Skill);
public sealed record AgentSkillsResponse(AgentSkill[] Skills);
public sealed record AgentSkillImportResponse(bool Imported, bool Reused, AgentSkill Skill);
public sealed record AgentTool(string Name, string Description, JsonElement InputSchema, string? Source = null,
    string? RawName = null, bool Enabled = true);
public sealed record McpResourceCapabilities(bool Resources, bool Templates);
public sealed record McpConnectionDiagnostic(string ServerId, string Transport, string State, string? Code = null,
    int ToolCount = 0, McpResourceCapabilities? ResourceCapabilities = null, DateTimeOffset? LastConnectedAt = null, long? Generation = null);
public sealed record McpConnectionsResponse(McpConnectionDiagnostic[] Connections);
public sealed record McpPresetPackage(string Registry, string Name, string Version, string? EntryPoint = null);
public sealed record McpPresetRequirement(string Name, string Type, bool Required, string Description);
public sealed record McpPreset(string Id, string Name, string Description, McpServerConfig Server, bool AlreadyConfigured,
    string SourceUrl, string[] Capabilities, string? ConfiguredServerId = null, McpPresetPackage? Package = null,
    string? License = null, string? Publisher = null, string? Network = null, McpPresetRequirement[]? Requirements = null,
    string[]? Notes = null, string? ConfigurationTemplate = null);
public sealed record McpCatalogResponse(McpPreset[] Presets, string[] ReusedCapabilities);
public sealed record AgentToolsResponse(AgentTool[] Tools, string[]? Errors = null, McpConnectionDiagnostic[]? Connections = null);
public sealed record AgentApprovalRequest(Guid ConversationId, Guid RequestId, string ToolCallId, Guid ApprovalId, bool Approved);
public sealed record AgentApprovalResponse(bool Approved, Guid ApprovalId, string ToolCallId);
