# C：模型与本地推理

更新日期：2026-10-05。[团队边界](../architecture/team-boundaries.md)。

## 代码范围

apps/model-gateway/models：连接/发现、能力、供应商与工具协议、流式、输出预算、context/history。apps/desktop/Services/Models：模型 API、SSE、预设与选择偏好。apps/shared/Chat：聊天 DTO；版本由 A 协调。本地模型脚本由 C 维护，进程集成/打包与 A 配合。

tool-protocols/tool-streaming 属模型映射；原生签名/加密续接与来源绑定保留，私有续接不能进入公开接口。

## 首轮交付

核对三协议、普通/工具流最终状态、取消、发现与凭据脱敏。总窗口、输入硬限、输出上限和记忆配额分开；大配置不代表供应商能力，未知代理/本地部署不猜额度。

context/history 只做请求投影，不截断 E 正式日志；保留完整有效轮次与调用/观察配对，不把公开思考当最终事实，也不注入兄弟聊天原文。当前 models/model-history.mjs 通过 data/tool-result-store.mjs 的 publicToolResult 读取公开结果预览，并使用 Platform 的摘要/身份合同；工具执行由 D 负责。Tools 仍允许引用 Models 的纯预算/协议辅助，Data 和 Platform 不反向依赖高层。

ChatStreamReader 与网关 SSE 同步核对事件、默认值、HTTP/机器错误码和取消。协议变化联系 A/B；工具字段联系 D；正式消息兼容联系 E。

## 验证

对应 protocols、streaming、output、model-budget/capabilities、context/model-history Node 测试，及 chat-stream、model-context、model-presets 客户端 smoke；模拟上游，不消耗真实模型 API。

本轮保留 UiText 依赖和公开合同。本地启停、下载校验/恢复另按需求推进，脚本启动不等于完整模型管理。
