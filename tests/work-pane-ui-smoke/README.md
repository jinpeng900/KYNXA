# Tabbed work pane native smoke

Build `dotnet build tests/work-pane-ui-smoke/WorkPaneUiSmoke.csproj -p:Platform=x64`, then run the built `WorkPaneUiSmoke.exe` on the Windows desktop. The fixture publishes its result path in `%TEMP%/kynxa-work-pane-latest.txt` and writes screenshots beside the result.

Links the production tab strip, screenshot panel, terminal panel and workspace header. Uses a separate native window, temporary synthetic Data/Extensions, fake archive API and generated PNG; no model request, real user terminal, project files or screen capture of other applications. Checks background tabs, exact selected resource routing, close/reopen without cancellation, hidden archive loading, late results, stop ownership, localization, narrow/wide layouts and horizontal tab scrolling. Production Shell routing and the original-resolution screenshot viewer remain covered by `screenshot-panel-ui-smoke`.

Screenshot tabs use the production archive identity and presentation priority. Routine captures stay catalog-only until explicitly opened. The fixture passes the screenshot catalog's real readiness into the shared tab state, so A→B→A restores the chosen image after deferred discovery without selecting a temporary terminal or showing another conversation's pixels.

截图标签沿用正式归档身份和展示优先级；普通截图只进入目录，明确打开后才读取。夹具把实际目录就绪状态传给共用标签状态，验证 A→B→A 的异步恢复不会误选临时终端，也不会显示其他聊天的图像。
