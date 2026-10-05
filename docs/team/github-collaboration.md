# 五人 GitHub 协作

更新日期：2026-10-06。功能交接基线 `0b3b23a`；具体文件和 RAG 分工见 [团队入口](README.md)。

采用现有功能模块分工和单仓库 PR 流程。每人有一个主要目录，桌面客户端与共享契约按同一领域归属，实际位置由 [主责清单](module-ownership.json) 维护。

| 人员 | 主要目录 | 配套范围 |
|---|---|---|
| A 队长 | apps/model-gateway/orchestration | platform、Integration、共享边界、构建和集成 |
| B 界面 | apps/desktop | 界面与 Presentation；Models/Tools/Data 客户端分别归 C/D/E |
| C 模型 | apps/model-gateway/models | desktop Services/Models、shared Chat |
| D 工具 | apps/model-gateway/tools | official-tools、ToolHost 功能、desktop Services/Tools、shared Tools |
| E 数据 | apps/model-gateway/data | desktop Services/Data、shared Memory |

不把跨端代码搬入个人姓名目录，也不拆成五个仓库。该选择与 [开源 Agent 的职责边界](../architecture/team-boundaries.md#一手源码证据) 一致；队长组合专业服务，各模块独立维护实现和测试。

## 权限的真实边界

目前仓库为个人账号下的 `jinpeng900/KYNXA`。个人仓库的协作者可以读写整个仓库，不能授予“只可提交一个文件夹”的权限。[GitHub 个人仓库权限](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/repository-access-and-collaboration/permission-levels-for-a-personal-account-repository)。

CODEOWNERS 指定审查人；启用主分支保护后要求负责人审查。生成规则让领域负责人或队长任一人可以批准，避免作者无法批准自己的 PR 时卡住。它不强制两人同时批准，也不限制读取目录。队长合并是本团队约定；当前个人仓库配置不保证其他有写入权的协作者绝对不能合并已满足规则的 PR。[CODEOWNERS 规则](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners)。

## 1. 队长邀请四位成员

先安装并登录 GitHub CLI，从仓库根运行。当前开发机没有可直接调用的 `gh`；未安装时先运行 `winget install --id GitHub.cli --exact`，安装后重新打开 PowerShell，再执行以下命令。把数组中的四项替换为真实 GitHub 用户名，不带 `@`。邀请由队长发送，需要对方接受；未自动执行。

```powershell
gh auth login
$kynxaMembers = @('界面成员用户名', '模型成员用户名', '工具成员用户名', '数据成员用户名')
foreach ($kynxaMember in $kynxaMembers) {
    gh api --method PUT "repos/jinpeng900/KYNXA/collaborators/$kynxaMember" -H 'Content-Length: 0'
    if ($LASTEXITCODE -ne 0) { throw "Invitation failed: $kynxaMember" }
}
```

此个人仓库无需传 `permission=push`；协作者已有仓库写入权，该 API 的自定义 permission 参数仅用于组织仓库。[邀请 API](https://docs.github.com/en/rest/collaborators/collaborators#add-a-repository-collaborator)。

查看已接受邀请的成员及权限：

```powershell
gh api repos/jinpeng900/KYNXA/collaborators --paginate --jq '.[] | {login, permissions}'
```

## 2. 填写真实目录审查人

当前 [.github/CODEOWNERS](../../.github/CODEOWNERS) 仅填写已知仓库所有者 `jinpeng900`。四位成员接受邀请后，以下脚本按同一份主责清单生成规则。先预览，确认用户名后加 `--write` 写入本机文件；脚本不联网、不邀请、不改变权限、不提交。

```powershell
node tools/development/configure-codeowners.mjs --lead jinpeng900 --desktop 界面成员用户名 --models 模型成员用户名 --tools 工具成员用户名 --data 数据成员用户名
# Add --write to the command above to save the verified local ownership rules.
# 在上面命令末尾加 --write，保存核对过的本机审查规则。
```

用户名必须是实际 GitHub login；占位中文会被脚本拒绝，不会写入无效规则。审查人需要仓库写入权限，脚本只做本地格式检查，不声称验证了 GitHub 账号或邀请状态。

## 3. 启用主分支保护

队长先将架构、CODEOWNERS 和 [Architecture workflow](../../.github/workflows/architecture.yml) 集成到 `main`，确认 GitHub 上 `Architecture guard` 检查成功，再启用保护。该工作流只检查架构边界，不替代模块功能测试、桌面构建和实际 UI 验收。

仓库 Settings → Branches → main：要求 PR、至少一次批准、Code Owners 审查、`Architecture guard` 检查，禁止强推和删除。仓库所有者保留管理员应急绕过；日常仍按 PR 流程。

也可以对尚未设置保护的 `main` 使用以下初始化命令。PUT 完整替换保护配置；已有保护时先读取并保留现有要求，不能直接用初始模板降低原规则。

```powershell
gh api repos/jinpeng900/KYNXA/branches/main/protection
# Use this initial policy only after checking the branch's existing protection.
# 核对已有保护后才使用这份初始配置。
gh api --method PUT repos/jinpeng900/KYNXA/branches/main/protection --input docs/team/main-branch-protection.json
```

如果 GET 返回 404，需区分未设置保护与当前账号无权访问；以仓库设置页或管理员权限核对。[分支保护 API](https://docs.github.com/en/rest/branches/branch-protection#update-branch-protection)。此配置的 `restrictions: null` 适配个人仓库，不宣传为“只有队长能推送/合并”的专属 ACL。

## 4. 成员日常操作

五个交接分支对应 A/B/C/D/E：`codex/team-a-integration`、`codex/team-b-ui`、`codex/team-c-models`、`codex/team-d-tools`、`codex/team-e-data`。它们从同一个包含最新交接说明的 `main` 提交建立，仅作为成员初次取代码的入口，不作为文件夹权限。已有分支不能用强推重置。

例如 B 首次下载并切到其交接分支：

```powershell
git clone https://github.com/jinpeng900/KYNXA.git
Set-Location KYNXA
git switch --track origin/codex/team-b-ui
```

后续每个任务从最新 `main` 建任务分支，避免长期交接分支积累已合并差异；不要直接向 `main` 推送。旧分支需同步重组后的目录。以下示例由 C 执行，B/D/E 分别使用 `codex/ui/`、`codex/tools/`、`codex/data/`，A 使用 `codex/integration/`。

```powershell
git fetch origin
git switch -c codex/models/context-budget origin/main

# Stage the files actually changed, including coordinated client/contract changes.
# 暂存实际修改的文件，含已协调的客户端和契约改动。
git add apps/model-gateway/models/实际文件.mjs
git commit -m "feat(models): 具体改动"
git push -u origin HEAD
gh pr create --base main --fill
```

新任务统一使用 `codex/` 前缀和领域名，既有 `feature/` 或 `integration/` 分支不需要批量改名。跨模块 PR 写明接口和双方调用端，负责人跑本模块验证，队长完成集成与合并。每个人的测试仍放现有 tests，不为人员分工复制测试框架。

本次交接文档和开发分支按用户授权提交/推送；不因此发送邀请或修改远端保护。四位成员账号尚未填写，CODEOWNERS 继续使用已知仓库所有者；保护配置只是初始化模板，未核验远端生效状态，不能声称已限制其他人合并。

## 验证入口

从仓库根运行 `node tools/development/check-architecture.mjs --details` 核对当前源码归属、相对引用与测试负责人，再执行 `git diff --check` 检查交接差异。新增或移动文件需验证两端引用和随包路径；文档更新不替代产品回归，历史验证数字不代表当前源码清单。邀请状态、GitHub Actions 和远端保护需分别核对，不能以本机模板或分支建立成功替代。
