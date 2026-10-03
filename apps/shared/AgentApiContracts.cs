using System.Text.Json;

namespace KYNXA.Contracts;

public sealed record ToolActivity(string ToolCallId, string Name, JsonElement? Arguments, string Status,
    string Summary, string? Result = null, Guid? ApprovalId = null, bool? OutsideWorkspace = null,
    string? Sandbox = null, string? WorkspaceRoot = null);

public sealed record McpServerConfig(string Id, string Name, string Command, string[] Args, bool Enabled,
    string? ProtocolVersion = null);
public sealed record AgentConfig(int Version, long Revision, McpServerConfig[] McpServers, string[] SkillDirectories);
public sealed record AgentConfigSaveRequest(int Version, long ExpectedRevision, McpServerConfig[] McpServers,
    string[] SkillDirectories);
public sealed record AgentSkill(string Id, string Name, string Description, string Source);
public sealed record AgentSkillDetail(string Id, string Name, string Description, string Source, string Content);
public sealed record AgentSkillResponse(AgentSkillDetail Skill);
public sealed record AgentSkillsResponse(AgentSkill[] Skills);
public sealed record AgentTool(string Name, string Description, JsonElement InputSchema, string? Source = null);
public sealed record AgentToolsResponse(AgentTool[] Tools, string[]? Errors = null);
public sealed record AgentApprovalRequest(Guid ConversationId, Guid RequestId, string ToolCallId, Guid ApprovalId, bool Approved);
public sealed record AgentApprovalResponse(bool Approved, Guid ApprovalId, string ToolCallId);
