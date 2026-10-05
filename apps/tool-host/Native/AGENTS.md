# Native ABI boundary / 原生 ABI 边界

- Preserve DllImport entry points, signatures, StructLayout, field order, packing, character sets, native constants and `KYNXA.ToolHost` namespace. Native identifiers follow Windows ABI conventions.
  保留 DllImport 入口、签名、StructLayout、字段顺序、对齐、字符集、原生常量与命名空间；原生命名遵守 Windows ABI。
- Keep approval, command selection, placement policy and handle ownership in channel implementations. Do not introduce elevated execution or foreground-lock bypasses here.
  审批、命令选择、位置策略和句柄所有权归通道实现；本目录不得引入提权执行或前台锁绕过。
- When files move, update linked smoke project source paths and `.editorconfig` native exceptions. Validate declarations with the pure input/decoder projects before any fixture-only native integration checks.
  移动文件时同步链接 smoke 源路径和 `.editorconfig` 原生例外；先用纯输入/解码工程验证声明，再按需要执行夹具原生集成检查。
