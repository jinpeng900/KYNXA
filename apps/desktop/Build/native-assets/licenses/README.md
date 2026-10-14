# 原生运行时许可证 / Native runtime licenses

这些文件保留对应固定 NuGet 包内许可证及第三方声明的原始字节。包版本、官方来源、包哈希、压缩包条目及各文件哈希记录在 [sources.json](sources.json)。

These files preserve the original license and third-party notice bytes from the pinned NuGet packages. [sources.json](sources.json) records each package version, official source, package hash, archive entry, and file hash.

`../native-runtime.json` 的 `localFiles` 将这些原文复制到原有随包路径，并继续校验大小与 SHA-256。构建不再为提取许可证下载两份共 219,703,478 字节的 NuGet 包；运行库仍由清单中的固定运行时包提供。

The `localFiles` entries in `../native-runtime.json` copy these texts to the existing payload paths with size and SHA-256 verification. Builds no longer download 219,703,478 bytes of NuGet archives solely for their licenses; the pinned runtime packages in the manifest continue to supply the binaries.

更新对应依赖版本时，维护者应从新版本的官方包提取原文，同时更新来源记录与运行时清单。不要改写、格式化或规范化许可证文件。

When updating a dependency, maintainers should extract its texts from the official package for that version and update both the provenance and runtime manifests. Do not edit, reformat, or normalize license files.
