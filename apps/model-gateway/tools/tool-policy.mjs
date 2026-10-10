import { randomUUID } from 'node:crypto';
import { toolFailure } from '../platform/tool-paths.mjs';

const READ_TOOLS = new Set(['filesystem.list', 'filesystem.read', 'filesystem.search', 'filesystem.stat', 'skill.list', 'skill.read',
  'tool.search', 'tool.load', 'tool.result.read', 'skill.resource.read', 'skill.inspect', 'skill.check',
  'conversation.history.search', 'conversation.history.read', 'knowledge.search', 'knowledge.read', 'knowledge.relations',
  'knowledge.plan', 'knowledge.assess', 'knowledge.experience', 'memory.read', 'memory.propose', 'web.search', 'terminal.host.read']);
const REVERSIBLE_TOOLS = new Set(['filesystem.write', 'filesystem.edit', 'filesystem.mkdir', 'work.folder.bind']);
const HOST_OBSERVATIONS = new Set(['computer.apps', 'computer.windows']);

export function needsToolApproval(context, name, { outsideWorkspace = false, verifiedSandbox = false, sensitiveRead = false } = {}) {
  if (context.permissionMode === 'full') return false;
  if (sensitiveRead) return true;
  // Typed observations and public text fetches cannot mutate a host or execute arbitrary shell commands.
  // 明确的只读枚举及受公共地址校验的网页读取不修改主机，也不执行任意脚本；不泛化到外部 MCP。
  if (name === 'web.fetch' || HOST_OBSERVATIONS.has(name)) return false;
  if (outsideWorkspace) return true;
  if (READ_TOOLS.has(name)) return false;
  if (context.permissionMode === 'smart' && (REVERSIBLE_TOOLS.has(name) || (['terminal.run', 'skill.run'].includes(name) && verifiedSandbox))) return false;
  // Unknown MCP annotations never grant authority. Deletion always requires approval in Ask/Smart.
  // 未知 MCP 注解不授予权限；Ask 和 Smart 模式中的删除始终需要审批。
  return true;
}

export class ToolApprovalRegistry {
  constructor({ timeoutMs = 5 * 60 * 1000 } = {}) { this.pending = new Map(); this.timeoutMs = timeoutMs; this.closed = false; }

  wait(context, call, { signal, emit, outsideWorkspace = false, summary = call.name }) {
    if (this.closed) return Promise.reject(toolFailure('工具服务已关闭。', 'TOOL_SERVICE_CLOSED', 409));
    if (this.pending.size >= 128) return Promise.reject(toolFailure('待审批工具已达上限。', 'TOOL_APPROVAL_CAPACITY', 409));
    const approvalId = randomUUID();
    return new Promise((resolve, reject) => {
      const finish = (approved, error) => {
        if (!this.pending.has(approvalId)) return;
        this.pending.delete(approvalId);
        clearTimeout(timer);
        signal?.removeEventListener('abort', aborted);
        if (error) reject(error); else resolve(approved);
      };
      const aborted = () => finish(false, toolFailure('工具审批已取消。', 'TOOL_CANCELLED', 409));
      const timer = setTimeout(() => finish(false, toolFailure('工具审批已过期。', 'TOOL_APPROVAL_EXPIRED', 409)), this.timeoutMs);
      this.pending.set(approvalId, { conversationId: context.conversationId, requestId: context.requestId, toolCallId: call.id, finish });
      if (signal?.aborted) return aborted();
      signal?.addEventListener('abort', aborted, { once: true });
      try {
        emit({ type: 'approval_required', tool: { toolCallId: call.id, name: call.name,
          arguments: structuredClone(call.arguments), status: 'approval-required', summary,
          approvalId, workspaceRoot: context.workspaceRoot, outsideWorkspace,
          reason: call.arguments.policy?.reason ?? call.arguments.reason ?? null } });
      } catch (error) { finish(false, error); }
    });
  }

  approve(input) {
    const approvalId = typeof input?.approvalId === 'string' ? input.approvalId.toLowerCase() : '';
    const value = this.pending.get(approvalId);
    if (!value) throw toolFailure('此工具审批不存在、已消费或已过期。', 'TOOL_APPROVAL_NOT_FOUND', 404);
    if (typeof input.approved !== 'boolean' || typeof input.conversationId !== 'string' || typeof input.requestId !== 'string' ||
        input.conversationId.toLowerCase() !== value.conversationId || input.requestId.toLowerCase() !== value.requestId || input.toolCallId !== value.toolCallId)
      throw toolFailure('审批身份不匹配，不能改变原工具调用。', 'TOOL_APPROVAL_MISMATCH', 409);
    value.finish(input.approved);
    return { approved: input.approved, approvalId, toolCallId: input.toolCallId };
  }

  close() {
    this.closed = true;
    for (const value of [...this.pending.values()]) value.finish(false, toolFailure('工具服务已关闭。', 'TOOL_SERVICE_CLOSED', 409));
  }

  cancelContext(context) {
    for (const value of [...this.pending.values()])
      if (value.conversationId === context?.conversationId && value.requestId === context?.requestId)
        value.finish(false, toolFailure('工具请求上下文已结束。', 'TOOL_CANCELLED', 409));
  }
}
