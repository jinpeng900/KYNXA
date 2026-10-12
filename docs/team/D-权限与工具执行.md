# D：权限与工具执行

更新日期：2026-10-12。[团队边界](../architecture/team-boundaries.md)；能力以 [工具架构](../architecture/agent-tools.md) 为准。交接分支：`kynxa_team/d-tools`。

## 代码范围

`apps/agent-core/src/tools/` 拥有注册/发现、策略审批、MCP/Skill、浏览器、执行与配置；`gateway_mcp.rs`、`gateway_browser.rs`、`gateway_web.rs`、`gateway_process.rs` 是实际 I/O 接线。`apps/official-tools/` 保留静态 `catalog.json`、`manifest.json`、Skills 与许可证；第三方原文不做批量改写。`apps/desktop/Services/Tools` 和 `apps/shared/Tools` 拥有客户端与 DTO。

`apps/tool-host/Desktop`、`Terminal`、`Sandbox`、`Native` 拥有 Windows 原生执行；根 Program/csproj 与打包由 A 协调。模型协议归 C，`orchestration/gateway_loop.rs` 与根 HTTP 路由归 A，结果/工作区持久化归 E；工具设置业务服务仍由 D 维护。

既有纯模块维护任务意图、审批策略、输入/配置/环境合同、浏览器目标、回执、副作用恢复与观察进展。组合根与任务状态由 A 维护；Rust 工具服务直接执行受控文件操作、搜索、配置/Skill 事务和 MCP stdio/HTTP/SSE 生命周期；终端/桌面实际操作仍经 broker/ToolHost。正式后端不依赖 Node SDK/NAPI。纯规则导入和真实跨域 I/O 按机器清单分别校验，修改需核对调用、权限快照与原生合同。

来源扫描在 `service_source_reading.rs`，网页读取与搜索在原生 web/source 服务，文档实际解析由 E 的 `retrieval/gateway_documents.rs` 接线并取得资源授额。扫描保持稳定排序、有界覆盖与真实后台取消；文本不能被文档资源等待全部堵住。普通网页搜索与明确操作本机浏览器的意图边界由 D 维护；来源归档联系 E，检索协调联系 A。失败返回可恢复观察，不能把未声明工具误调用变成整轮聊天中断；不重复已有文件、终端或浏览器实现。

## 维护与交付

核对官方资源路径、Skill、MCP 启动/刷新/退出、浏览器会话、ToolHost 定位与请求合同。执行使用每请求固定的工作范围与权限快照；模型参数、MCP 注解和 Skill 内容不扩大授权。

审批绑定单次调用；取消、过期与范围变化后不得执行。保留 call ID、round/order，结果可按完成时间返回但不改身份。文件代理、AppContainer、Host Terminal 与配置 MCP 程序的边界各按实现说明。

回执区分完成、失败、中断、未派发与不确定；失败不能伪装成功。结果归档/分页与 E 协作，B 负责审批和结果 UI。

## 验证

受影响 Rust 工具/策略/回执测试、实际本机模拟 MCP/HTTP/浏览器和扫描取消夹具，以及 ToolHost 构建与 Windows 实际验证。独立临时目录与虚构环境，记录真实进程/作业退出和资源归还。旧 Node 套件成绩不替代当前原生执行验证。

已实现回执不代表崩溃后安全重放。长任务恢复先核实已发生动作，不能默认重试有副作用操作。
