# Sandbox boundary / 沙箱边界

- Preserve manifest hashes, bounded snapshot limits, link/hard-link rejection, configured exclusions and exact managed-workspace exception checks. Never relax user workspace ACLs or write results back implicitly.
  保留清单哈希、快照上限、链接/硬链接拒绝、配置排除项与严格托管工作区例外；不放宽用户工作区 ACL，不隐式写回结果。
- Keep AppContainer token verification before process resume, non-inherited Job ownership, deadlines and resource cleanup. Interrupted execution remains uncertain; sandbox failures cannot fall back to host execution.
  保留进程恢复前的 AppContainer 令牌校验、非继承 Job 所有权、期限与资源清理；中断执行维持不确定语义，沙箱失败不得回退到宿主执行。
- Keep request/result contracts and `KYNXA.ToolHost` namespace compatible; test with independent temporary workspaces, never user Data or actual credentials.
  保持请求/结果合同和命名空间兼容；验证使用独立临时工作区，不使用用户 Data 或真实凭据。
