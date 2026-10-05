# D：权限与工具执行

更新日期：2026-10-06。[团队边界](../architecture/team-boundaries.md)；能力以 [工具架构](../architecture/agent-tools.md) 为准。交接分支：`codex/team-d-tools`。

## 代码范围

apps/model-gateway/tools：注册/发现、策略审批、MCP/Skill、浏览器、执行适配与配置。official-tools 保留原目录；自有 Tools/catalog.mjs 的导入已随重组调整，第三方技能原文和许可证未改，不承诺整个包逐字节不变。apps/desktop/Services/Tools 和 apps/shared/Tools 拥有客户端与 DTO。

apps/tool-host/Desktop、Terminal、Sandbox、Native 拥有 Windows 原生执行；根 Program/csproj 与打包由 A 协调。模型协议归 C，工具循环与 agent-http-routes 的 HTTP 接线归 A，结果/工作区持久化归 E；工具设置业务服务仍由 D 维护。

`tools/retrieval/` 的 web-search、source-reader、web-source-normalizer 与 descriptors 归 D。普通网页搜索与用户明确要求操作本机浏览器的意图边界由 D 维护；来源归档与证据窗口联系 E，检索协调联系 A。工具执行失败返回可恢复的观察，不能把未声明工具误调用变成整轮聊天中断；不重复内置已有文件、终端或浏览器实现。

## 首轮交付

核对官方资源路径、Skill、MCP 启动/刷新/退出、浏览器会话、ToolHost 定位与请求合同。执行使用每请求固定的工作范围与权限快照；模型参数、MCP 注解和 Skill 内容不扩大授权。

审批绑定单次调用；取消、过期与范围变化后不得执行。保留 call ID、round/order，结果可按完成时间返回但不改身份。文件代理、AppContainer、Host Terminal 与配置 MCP 程序的边界各按实现说明。

回执区分完成、失败、中断、未派发与不确定；失败不能伪装成功。结果归档/分页与 E 协作，B 负责审批和结果 UI。

## 验证

受影响 tools、routing、lifecycle、parallel、MCP、skill、browser、filesystem/runner 测试；ToolHost 构建及对应 Windows 实际验证。独立临时目录和模拟上游/MCP。

已实现回执不代表崩溃后安全重放。长任务恢复先核实已发生动作，不能默认重试有副作用操作。
