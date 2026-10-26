# 桌面构建

构建并启动 Windows x64 桌面软件，复用本机已有工具链和运行资源。

可选参数 `-NativeRuntimeRoot <短绝对目录>` 为资源准备、复用和构建指定同一缓存目录，默认仍使用仓库缓存。VS 可在本机 `KYNXA.Desktop.csproj.user` 中设置 `KynxaNativeRuntimeRoot`；此选项不改变 Full 载荷或最终部署目录，个人路径不进入源码。
