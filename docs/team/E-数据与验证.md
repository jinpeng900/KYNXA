# E：数据与验证

更新日期：2026-10-12。[团队边界](../architecture/team-boundaries.md)；[正式数据约定](../architecture/chat-work-memory.md)。交接分支：`kynxa_team/e-data`。

## 代码范围

`apps/agent-core/src/storage/` 拥有正式会话/记忆/结果归档、Data 与扩展路径及迁移、sandbox-workspaces、日志/SQLite 事务；`retrieval/` 拥有索引、来源与后台作业。`storage/gateway.rs`、`gateway_memory.rs` 和 `retrieval/gateway_*.rs` 直接执行原生服务的 API 接线，不使用 Node/NAPI。`apps/desktop/Services/Data` 拥有会话/记忆 API 与路径迁移，`apps/shared/Memory` 拥有 DTO。

纯 storage 规则沿机器清单只允许 storage/retrieval 边界，纯 retrieval 不引入其他业务域；不得引用 I/O 集成模块。真实存储/检索接线按 `integrationModules` 的逐文件 `allowedDomains` 复用 C 的模型服务、D 的读取服务和 A 的资源准入/分段合同，不重新维护这些业务规则。工具结果/工作区事实由 E 管理，工具执行由 D 管理，请求投影由 C 管理。

分块/候选/查询规则、证据账本、数据/设置/记忆合同和候选记忆提取留在既有模块。正式 JSONL、SQLite、资料快照与迁移由 Rust 数据服务直接执行；网关维持单一事务入口、Raw UTF-16/原始 JSON 哈希身份、队列和版本冲突，桌面缓存不能新增写入者。

`retrieval/gateway_index.rs`、`gateway_embedding.rs`、`gateway_documents.rs`、`gateway_structure.rs` 与 `gateway_tools.rs` 拥有 SQLite/ANN 查询、向量发布、后台索引、文档/语法解析、检索设置与证据窗口的实际执行。配套 `apps/desktop/Services/Data/RetrievalApiClient.cs` 与 `apps/shared/Memory/RetrievalApiContracts.cs` 同归 E。嵌入/重排使用 C 的真实批接口与 A 的授额，驻留租约在确认模型卸载后归还；重建索引不得删除来源原文或正式聊天。

## 维护与交付

核对单网关正式写入、队列、JSONL、catalog 和索引投影、初始化及迁移。未发草稿不保存；桌面缓存不能覆盖助手正式状态；稳定 ID、原文与调用/结果事件保持。

冷启动目录仅读元信息，正文按页读取；游标绑定日志版本、归属与范围。页预算不切开消息，取消传播到原生等待/读取，私有续传只能由内部 scope/owner/hash 校验入口回读。任务结果状态与正式消息、HTTP/SSE、历史/摘要投影一致，不把传输完成当成核验成功。

catalog 为目录权威，project.json 为投影；context 为可重建摘录而非确认长期事实。记忆范围、用户确认、expectedRevision 与来源状态保留。损坏/未来版本明确失败并保护原文件，不视为空数据覆盖。

路径来自配置/统一模块；迁移验证后切换指针并保留恢复来源。大工具结果保留完整引用/分页，公开消息不暴露私有内部记录。工作区存储不承担权限决策。

## 验证

对应 Rust 历史分页/正式存储/迁移/记忆/来源发布/SQLite/ANN 测试，以及 conversation-store、storage-migration、gateway-response 客户端 smoke；初始化前指定独立临时 Data/虚构连接。性能小样本固定来源、范围、向量与冷热条件，不将历史 Node 成绩或不同硬件推断为本轮收益。

E 汇总数据证据，各成员负责自己的测试，A 集成。既有会话保存、检查点和回执分别验证；执行未知或已完成副作用不得因重启而自动重新派发，不能据归档存在宣称完整恢复系统已完成。
