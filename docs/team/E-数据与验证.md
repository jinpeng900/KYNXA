# E：数据与验证

更新日期：2026-10-06。[团队边界](../architecture/team-boundaries.md)；[正式数据约定](../architecture/chat-work-memory.md)。交接分支：`kynxa_team/e-data`。

## 代码范围

apps/model-gateway/data：conversation、memory、索引、Data/扩展路径及迁移、sandbox-workspaces、tool-result-store。apps/desktop/Services/Data：会话/记忆 API 与路径迁移。apps/shared/Memory：记忆 DTO。

Data 只依赖自身和 Platform，不能导入模型预算、执行服务或 HTTP 路由。工具结果/工作区事实由 E 管理，执行由 D 管理，请求投影由 C 管理。

`data/retrieval/` 归 E：SQLite 混合索引、分块、来源库、索引作业、检索设置与证据引用/窗口。配套 `apps/desktop/Services/Data/RetrievalApiClient.cs` 与 `apps/shared/Memory/RetrievalApiContracts.cs` 同归 E。C 的嵌入/重排通过上层协调接入，不能让索引层反向依赖模型；重建索引不得删除来源原文或正式聊天。

## 首轮交付

核对单网关正式写入、队列、JSONL、catalog 和索引投影、初始化及迁移。未发草稿不保存；桌面缓存不能覆盖助手正式状态；稳定 ID、原文与调用/结果事件保持。

catalog 为目录权威，project.json 为投影；context 为可重建摘录而非确认长期事实。记忆范围、用户确认、expectedRevision 与来源状态保留。损坏/未来版本明确失败并保护原文件，不视为空数据覆盖。

路径来自配置/统一模块；迁移验证后切换指针并保留恢复来源。大工具结果保留完整引用/分页，公开消息不暴露私有内部记录。工作区存储不承担权限决策。

## 验证

对应 conversation/index、data-layout、memory、storage/migration、tool-result-store、sandbox-workspaces Node 测试，以及 conversation-store、storage-migration、gateway-response 客户端 smoke。初始化前指定独立临时 Data/虚构连接。

E 汇总数据证据，各成员负责自己的测试，A 集成。持久任务、检查点、恢复与验证版本绑定另立合同，会话保存/归档不等于完整恢复系统。
