# 同仓库分支写入权限

当前用户约定：`hui33844` 直接推送 `jinpeng900/KYNXA` 的 `kynxa_team/b-ui`；是否纳入 `main` 由 `jinpeng900` 决定，不使用 Fork 作为当前交接方式。

GitHub Rulesets 支持个人账号作为 bypass actor。这里的三个独立规则组合使用，不能只启用界面写入规则而遗漏其他分支保护。

2026-10-06 已创建并通过 REST 回读核对：其他分支规则 `24560033`、界面写入规则 `24560035`、界面历史规则 `24560038` 均为 `active`。已核对 `main`、界面与模型分支匹配规则及账号名单；成员尚未接受邀请，未声称已用其账号实际推送验收。

| 配置 | 匹配范围 | 生效约束 |
|---|---|---|
| `owner-branches.json` | 所有分支，排除 `kynxa_team/b-ui` | 仅 `jinpeng900` 可创建、更新、删除或强推；包含 `main`、其余成员分支与新分支 |
| `ui-writers.json` | `kynxa_team/b-ui` | 仅 `jinpeng900` 与 `hui33844` 可创建/更新 |
| `ui-history.json` | `kynxa_team/b-ui` | 仅 `jinpeng900` 可删除或强推；界面负责人的写入绕过不适用于这份独立规则 |

公开账号 ID：`204304556` 为 `jinpeng900`，`288504216` 为 `hui33844`。Ruleset bypass 不会授予仓库权限；成员仍须接受协作者邀请后才能推送。仓库所有者保留全部分支的决定权。其他成员尚未授权，将来分配其分支时需同时调整排除范围与对应分支写入人规则，不能只增加协作者权限。

规则限制分支写入，不限制该分支内只能编辑界面文件，也不限制公开仓库读取或他人创建 PR。跨域修改仍由主责审查和队长验收。界面负责人可以向 `main` 提 PR，但无法自行将其合并进受限制的 `main`。

## 应用与核查

队长在仓库根执行。先读取已有规则，不能覆盖无关规则；三个名称不存在时使用 POST 创建。重复执行必须先按名称查找 ID，再决定是否更新已有同名规则，避免创建重复规则。

```powershell
gh api repos/jinpeng900/KYNXA/rulesets --paginate

gh api --method POST repos/jinpeng900/KYNXA/rulesets --input docs/team/rulesets/owner-branches.json
gh api --method POST repos/jinpeng900/KYNXA/rulesets --input docs/team/rulesets/ui-writers.json
gh api --method POST repos/jinpeng900/KYNXA/rulesets --input docs/team/rulesets/ui-history.json
```

全部规则确认 `enforcement: active`，且 actors、目标分支和规则类型与文件一致后，才发送邀请：

```powershell
gh api --method PUT repos/jinpeng900/KYNXA/collaborators/hui33844 -H 'Content-Length: 0'
```

读取匹配规则用于验收；分支名中的 `/` 编码为 `%2F`：

```powershell
gh api repos/jinpeng900/KYNXA/rules/branches/main
gh api repos/jinpeng900/KYNXA/rules/branches/kynxa_team%2Fb-ui
gh api repos/jinpeng900/KYNXA/rules/branches/kynxa_team%2Fc-models
```

读取规则集详细端点还需核对 bypass actors；规则清单返回成功不等于已用成员账号实际验证推送。实际成员验收应确认：正常 UI 分支推送成功；`main`、其他分支写入与 UI 删除/强推被拒绝，不为验收改写正式提交或强推真实分支。

上游依据：[指定个人绕过规则](https://github.blog/changelog/2026-05-07-repository-rulesets-user-bypass-and-branch-renaming/)、[规则集更新限制](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)、[REST 合同](https://docs.github.com/en/rest/repos/rules?apiVersion=2026-03-10)。旧式分支保护的用户推送名单有不同适用范围，不能据此推断 Rulesets 不支持本方案。
