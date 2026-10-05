# platform

主责：A 协调 · 共用基础。

维护跨域纯合同、ID、原子文件、消息/结果投影、计时与路径基础。专业语义变更联系对应 C/D/E 负责人。

不依赖 Models、Tools、Data、Orchestration。这里只收实际复用的基础；不能放模型连接、业务仓储、工具调度或界面状态。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。
