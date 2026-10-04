# Tabbed work pane native smoke

Build `dotnet build tests/work-pane-ui-smoke/WorkPaneUiSmoke.csproj -p:Platform=x64`, then run the built `WorkPaneUiSmoke.exe` on the Windows desktop. The fixture publishes its result path in `%TEMP%/kynxa-work-pane-latest.txt` and writes screenshots beside the result.

Links the production tab strip, screenshot panel, terminal panel and workspace header. Uses a separate native window, temporary synthetic Data/Extensions, fake archive API and generated PNG; no model request, real user terminal, project files or screen capture of other applications. Checks background tabs, exact selected resource routing, close/reopen without cancellation, hidden archive loading, late results, stop ownership, localization, narrow/wide layouts and horizontal tab scrolling. Production Shell routing and the original-resolution screenshot viewer remain covered by `screenshot-panel-ui-smoke`.
