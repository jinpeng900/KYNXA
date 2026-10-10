import { isAbsolute } from 'node:path';
import { bindLocalPath, inspectLocalPath, resolveToolPath, toolFailure } from '../platform/tool-paths.mjs';

export const workDirectoryDescriptor = {
  name: 'work.folder.bind', source: 'builtin',
  description: 'Associate an existing directory with this chat\'s current work for ongoing work and automatic incremental indexing. Use when the user asks to keep working in a folder, not for a one-time read. This persists the work binding; never creates or moves files, never changes chat ownership. Existing bindings are replaced only when the user requests that change. The current turn retains its original file permissions; use absolute paths with reason until the next turn.',
  inputSchema: { type: 'object', properties: {
    path: { type: 'string', minLength: 1, maxLength: 4096 },
    reason: { type: 'string', minLength: 1, maxLength: 2000 }
  }, required: ['path', 'reason'], additionalProperties: false }
};

/** Validate the persistent target independently of the current turn's immutable workspace.
 * 持久目录目标单独验证；不通过目录关联改写当前轮次的不可变文件权限。 */
export async function prepareWorkDirectory(context, input, boundary) {
  if (!context.projectId) throw toolFailure('当前聊天不属于工作，请先创建工作后关联目录。', 'WORK_REQUIRED', 409);
  if (!input.reason.trim()) throw toolFailure('请说明关联目录的长期工作用途。', 'WORK_BINDING_REASON_REQUIRED');
  if (!isAbsolute(input.path)) throw toolFailure('关联工作目录需要明确的绝对路径。', 'WORK_BINDING_PATH_REQUIRED');
  const target = resolveToolPath(context, input, [], { deferScopeCheck: true });
  if (boundary.isOwned(target.path) || boundary.isReadOnlyExtension(target.path))
    throw toolFailure('应用存储和扩展目录不能作为外部工作目录。', 'PROTECTED_APP_DATA', 403);
  // Explicitly mounted directories stay link-free; transient file reads have their own binding contract.
  // 明确挂载的目录保持无链接结构，临时文件读取继续使用其独立路径绑定合同。
  const info = await inspectLocalPath(target.path);
  if (!info?.isDirectory()) throw toolFailure('指定的工作路径不是文件夹。', 'WORK_BINDING_NOT_DIRECTORY');
  const binding = await bindLocalPath(target.path, {
    workspaceRoot: context.workspaceDiagnostic ? undefined : context.workspaceRoot
  });
  if (boundary.isOwned(binding.path) || boundary.isReadOnlyExtension(binding.path))
    throw toolFailure('应用存储和扩展目录不能作为外部工作目录。', 'PROTECTED_APP_DATA', 403);
  return binding;
}
