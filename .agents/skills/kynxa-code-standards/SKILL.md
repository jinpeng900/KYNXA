---
name: kynxa-code-standards
description: "编写、审查或规范化 KYNXA 的 C#、JavaScript/ESM 和 PowerShell 代码时使用，统一语义命名、双语说明注释和格式，检查公开契约与行为兼容。不用于修改第三方原文、模型回复或无关仓库。"
---

# KYNXA 代码规范

本 Skill 定义 KYNXA 的代码书写规则，与 `kynxa-development` 的模块、交互和验证约定配合使用。用户当前要求优先；规范化不授予发布权限，也不要求整仓重写。

## 工作入口

1. 检查已有改动，定位声明、作用域、使用者及序列化/反射/绑定边界。修改前读取 [命名与双语注释](references/naming-comments.md) 的对应语言规则。
2. 为含义不清的内部名称给出实际业务含义，再在同一作用域修改声明与引用。不能用全局文本替换来改变量；不为统一大小写改公开协议。
3. 保持模块职责。纯规范化只调整名称、注释和必要格式；发现逻辑缺陷时单独记录和处理，不混入无法核对的行为变化。
4. 复查差异中的字符串、对象键、导出、绑定、原生布局和异步顺序。调用与结果配对、权限快照、原始日志和版本冲突规则保持兼容。
5. 按受影响链路编译并运行现有验证。批量命名改动运行对应完整套件；注释或规范文件改动不新增镜像测试。完成后报告改名例子、实际验证及未覆盖部分。

## 命名判断

- 名称说明对象、动作或状态；数量注明单位，例如 `usedTokens`、`timeoutMs`、`stdoutBytes`。布尔值优先 `is` / `has` / `can` / `should`，避免需要猜测的 `flag`、`data2`。
- C# 用 PascalCase 类型/方法/属性、camelCase 参数/局部变量、`_camelCase` 私有实例字段。JavaScript 用 lowerCamelCase 函数/变量、PascalCase 类；真正固定的模块上限用 UPPER_SNAKE_CASE。PowerShell 函数使用批准动词加有意义名词。
- `i`、`index`、`x/y`、通用集合 `T`、弃用参数 `_`、标准 Win32 名称等含义明确时保留，不为了字符数扩写。
- JSON/schema 字段、记录属性、工具名、错误码、事件类型、稳定 ID、环境变量、导出和 XAML/WebView 绑定都是兼容边界。需要更清晰的局部名称时使用别名，例如 `{ id: conversationId }`；返回对象必须保留原键。

## 双语注释

- 项目自有说明性英文注释保留英文，在同一块或紧邻位置增加中文说明。新写的非平凡说明使用中英两层，解释约束、原因、生命周期或恢复条件。
- 中文要与真实行为一致，不能把“已发送”解释成“成功”，或把“目录发现”解释成“运行环境已验收”。示例和 XML/JSDoc 格式见引用文件。
- 不逐行翻译简单代码、不填充空白注释。许可证、版权、第三方原文、生成文件、分析器指令以及代码/数据字符串中的注释样文本保留原样。

## 格式与适用范围

以仓库 `.editorconfig` 为准：UTF-8、CRLF、末尾换行、空格缩进；C#/PowerShell 4 空格，JS/ESM 和 JSON/YAML 2 空格。局部编辑遵循所在文件风格，不顺便全文件排版。兼容 Windows PowerShell 5.1 的历史中文脚本保留已声明的 UTF-8 BOM；原始字符串内容不能因排版改变。第三方包的原始字节和发布哈希不受此规范化修改。

详细的项目选择、例外与一手来源见 [命名与双语注释](references/naming-comments.md)。模块边界见 [代码与模块规范](../kynxa-development/references/coding-standards.md)；验证选择见 [验证与协作](../kynxa-development/references/validation.md)。
