# data

主责：E · 会话与记忆。

维护正式 JSONL、项目/聊天归属、确认记忆、索引、配置路径、初始化/迁移、工具原文归档与托管工作区。

只引用本领域和 Platform 及外部库；不依赖 Models、Tools、Orchestration。单网关拥有正式写入，保留稳定身份、范围隔离与并发冲突规则。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。
