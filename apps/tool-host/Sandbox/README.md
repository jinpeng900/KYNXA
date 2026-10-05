# Sandbox / 沙箱通道

`AppContainerRunner` owns per-run AppContainer profiles, Job limits, staging security, cancellation and descendant cleanup. `WorkspaceSnapshot` selects bounded workspace copies; `SkillSnapshot` verifies the approved file manifest and hashes before copying a read-only package.
`AppContainerRunner` 拥有每次运行的 AppContainer 配置、Job 限制、暂存安全、取消与子进程清理；`WorkspaceSnapshot` 选择受限工作区副本，`SkillSnapshot` 在复制只读包前校验已批准清单与哈希。

Input configuration supplies source paths and exclusions. Copying must preserve source files and exclude application Data, links and credentials; managed-workspace exceptions remain narrowly scoped. The gateway owns retained-stage cleanup after the tool turn. Interop lives in `../Native`; namespace remains `KYNXA.ToolHost`.
源路径与排除项来自输入配置；复制保留源文件并排除应用 Data、链接与凭据，托管工作区例外维持严格范围。网关负责工具轮次后保留暂存区的清理。原生声明位于 `../Native`，命名空间保持不变。

See [sandbox fixtures](../../../tests/sandbox-smoke/README.md). Run process/security checks only with independent temporary workspaces and simulated credentials.
验证参见沙箱夹具说明；进程与安全检查仅使用独立临时工作区和模拟凭据。
