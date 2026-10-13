# A：队长与任务编排

更新日期：2026-10-12。[团队边界](../architecture/team-boundaries.md)。交接分支：`kynxa_team/a-integration`。

## 代码范围

`apps/agent-core/src/gateway.rs`、`gateway_contract.rs` 与 `bin/kynxa-gateway.rs` 拥有 Windows 原生网关 HTTP/SSE、配置和入口。`orchestration/` 拥有任务循环、预算、恢复、来源核验与任务结果；`resources/` 接入独立资源服务的真实准入、租约与外部模型预测。`apps/desktop/Services/Integration` 拥有原生网关启动、生命周期、响应与错误处理。

A 维护 `apps/agent-core/src/lib.rs`、`main.rs`、共享请求取消与身份合同，以及上述组合/编排/资源模块；C 维护 models，D 维护 tools，E 维护 retrieval/storage。正式启动为随包 `runtime/agent/kynxa-gateway.exe`，初始化沿原生 `--initialize-storage`；不保留 Node/NAPI 启动或故障回退。根项目文件、构建、打包与跨端版本由 A 协调，专业规则与 C/D/E 核对。

`orchestration/gateway_loop.rs` 组合检索、模型与工具服务，协调候选、证据回读和请求投影。随包嵌入/重排资源由 C 提供，A 维护 `apps/desktop/Build/RuntimePackaging.targets` 与原生 PowerShell 构建接线；E 维护索引版本，D 维护网页工具，B 维护设置与来源显示。`apps/mock-backend/`、`apps/desktop-preview/` 及根 `Directory.Build.props/targets` 由 A 负责开发集成，不作为正式后端。

维护 `.github/`、目录审查规则生成及 PR 集成流程。各成员拥有仓库协作权限，目录主责通过 CODEOWNERS 审查体现；命令和生效步骤见 [GitHub 协作](github-collaboration.md)。队长维护共享边界，不能因负责合并就代替领域负责人完成其全部实现与测试。

## 维护与交付

检查 Rust 模块引用、资源定位、CLI/环境配置、公开 API、发布资源与 smoke 编译链接。纯规则保持机器清单的域依赖与禁止循环，不引用 I/O 集成模块；真实执行按 `integrationModules`/`executionModules` 逐文件声明。组合根调用专业服务，不把供应商参数、文件事务或工具实现重新集中到任务循环。

核对 request/conversation 身份、取消、迟到结果、工具调用/结果和单次批准身份。模型流由 C 适配，执行事实由 D 产生，正式写入由 E 实施，A 协调生命周期与 SSE；B 负责页面接线。

## 验证

受影响 Rust 单元/本机 HTTP/SSE 夹具、客户端 smoke、桌面/ToolHost 构建与架构检查；影响面广时再扩大。旧 Node 套件保留为迁移前历史证据，不能作为当前验证。记录真实 HTTP 尝试、取消、回执和源码版本；编译与目录重组不等于全部功能复现。

任务持久化、检查点与已发生副作用按实际合同分别验收；执行未知状态不能自动重试，已有上下文恢复不能代表任意长任务自动安全恢复。不承诺未经验证的完成时长。
