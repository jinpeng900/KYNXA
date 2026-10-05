# A：队长与任务编排

更新日期：2026-10-05。[团队边界](../architecture/team-boundaries.md)。

## 代码范围

apps/model-gateway/orchestration 拥有 runtime、tool-loop、tool-run、server 实现、agent-http-routes 工具设置 HTTP 接线和 http-transport；platform 拥有跨模块合同与原子 JSON、ID、assistant-segments、model-transcript、reply-timing、tool-paths、tool-excerpts 等基础。apps/desktop/Services/Integration 拥有网关启动、生命周期、响应与错误处理。

根 server.mjs、initialize-storage.mjs、migrate-storage.mjs 保留 CLI/导出兼容入口，实现下沉。根项目文件、构建、打包与跨端版本由 A 协调；专业规则与 C/D/E 核对。

维护 `.github/`、目录审查规则生成及 PR 集成流程。各成员拥有仓库协作权限，目录主责通过 CODEOWNERS 审查体现；命令和生效步骤见 [GitHub 协作](github-collaboration.md)。队长维护共享边界，不能因负责合并就代替领域负责人完成其全部实现与测试。

## 首轮交付

检查相对导入、资源定位、CLI 参数、发布资源与 smoke 编译链接。Models/Tools/Data/Platform 不引用 Orchestration；Data 只依赖自己与 Platform。组合根注入专业服务，不把供应商参数、文件事务或工具实现重新集中到 runtime。

核对 request/conversation 身份、取消、迟到结果、工具调用/结果和单次批准身份。模型流由 C 适配，执行事实由 D 产生，正式写入由 E 实施，A 协调生命周期与 SSE；B 负责页面接线。

## 验证

完整网关套件、受影响客户端 smoke、桌面/ToolHost 构建与依赖检查。记录实际结果；目录重组不等于全部业务分层完成。

持久任务、检查点、自动修复和恢复另立交付；本轮不新增独立 Host/Rust 权限服务，也不承诺完成时长。
