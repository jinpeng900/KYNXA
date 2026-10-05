# models

主责：C · 模型与上下文。

维护云端/本地模型协议、能力/预算、流读取、上下文与历史配对投影、模型配置。

可读取 Data 的配置路径和结果公开投影，复用 Platform；不依赖 Tools 执行服务或 Orchestration。历史压缩只修改请求投影，不修改正式原文。

参见 [五人职责](../../../docs/team/README.md)、[依赖与合同](../../../docs/architecture/team-boundaries.md)。根目录 `server.mjs`、`initialize-storage.mjs`、`migrate-storage.mjs` 为稳定启动入口；不要在本目录再创建另一份正式服务或数据源。

跨域变更同步生产调用方、共享 DTO、测试和随包清单。新增/移动模块后运行 `node tools/development/check-architecture.mjs`；回归选择见 [开发规则](../../../.agents/skills/kynxa-development/SKILL.md)。
