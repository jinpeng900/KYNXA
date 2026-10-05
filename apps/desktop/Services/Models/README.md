# 模型与聊天客户端 / Model and chat clients

负责人 / Owner: **C**。

模型连接 API、SSE 读取、目录与预设、模型选择偏好，以及兼容测试用 MockChatClient。 / Model connection API, SSE parsing, catalog, presets, selection preferences and the legacy test MockChatClient.

通过 Integration 连接网关，复用共享 Chat 与 Tools 契约；模型选择只保存 UI 偏好。 / Reach the gateway through Integration and use shared Chat and Tools contracts; model selection stores UI preferences only.

本次目录整理保留原命名空间、公开类型及协议；已有客户端对 UiText 等展示类型的依赖仍保留。
The directory reorganization preserves namespaces, public types and protocols; existing client dependencies on presentation types such as UiText remain.
