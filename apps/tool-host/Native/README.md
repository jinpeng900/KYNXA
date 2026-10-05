# Native / Windows 原生声明

`NativeMethods` declares process, Job, token and AppContainer interop; `DesktopNativeMethods` declares desktop/window, input and capture interop. Both retain the `KYNXA.ToolHost` namespace and serve the channel implementations in the same assembly.
`NativeMethods` 定义进程、Job、令牌与 AppContainer 声明；`DesktopNativeMethods` 定义桌面/窗口、输入与截图声明。两者保持原命名空间，为同一程序集中的通道实现服务。

This folder owns signatures and ABI layouts, including structure sizes, field order, packing, character sets, constants and marshaling. Channel runners own policy and resource lifetime; moving declarations must not alter those contracts. Linked pure input and terminal decoder smoke projects compile these exact declarations without performing desktop or sandbox actions.
本目录负责签名与 ABI 布局，包括结构大小、字段顺序、对齐、字符集、常量与封送；策略和资源生命周期归通道运行器。移动声明不得改变合同；链接源码的纯输入和终端解码 smoke 编译这些声明而不执行桌面或沙箱操作。
