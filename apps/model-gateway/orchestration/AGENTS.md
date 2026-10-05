# orchestration 开发归属

主责：A · 编排与集成。沿用仓库根开发 Skill 与代码规范。

组合 Models、Tools、Data；维护 HTTP 生命周期、模型与工具循环、取消、回执提交和运行预算。

允许组合各领域；Models、Tools、Data、Platform 不反向引用本目录。业务规则由领域服务维护，不能堆入 server 或 runtime。

普通修改不增加审批步骤。跨目录协作说明接口与文件归属，保留已有改动；验证不能用历史记录代替。目录和源码路径只是实现位置，不得写入用户数据或替代稳定 ID。
