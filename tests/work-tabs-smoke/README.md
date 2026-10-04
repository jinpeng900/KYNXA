# Work tab state smoke

Run `dotnet run --project tests/work-tabs-smoke/WorkTabsSmoke.csproj` from the repository root.

Links the production pure tab state. Checks background additions, explicit selection, logical close/reopen, per-conversation state, source ordering/filtering and bounded LRU recovery. Uses synthetic IDs and no disk data or model requests. Native rendering and archive lifecycle are checked separately in `work-pane-ui-smoke`.
