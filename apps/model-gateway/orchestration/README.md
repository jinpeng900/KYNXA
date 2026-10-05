# orchestration

主责：A · 编排与集成。

组合 Models、Tools、Data；维护 HTTP 生命周期、模型与工具循环、取消、回执提交和运行预算。

允许组合各领域；Models、Tools、Data、Platform 不反向引用本目录。业务规则由领域服务维护，不能堆入 server 或 runtime。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。
