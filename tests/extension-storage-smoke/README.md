# Extension storage smoke

Run from the repository root:

```powershell
dotnet run --project tests/extension-storage-smoke/ExtensionStorageSmoke.csproj
```

The default mode uses temporary roots and explicit pointer paths. It verifies extension location precedence, source-preserving copies, SHA-256 integrity, owned MCP paths and cache variables, skill disable IDs, pointer/version conflicts, cancellation, Windows links, and the relationship between independent extensions and Data migration. It does not initialize WinRT StoragePaths or access the user's Data, API keys or MCP servers.

An explicit `--source <root> --target <empty-root> --pointer <extensions.json>` mode invokes the production migration service and prints only the resulting root and verified file count. Its caller must already hold the shared `storage-migration.lock` and verify that the gateway has entered maintenance with no active requests. The service's separate operation lock prevents competing copies; it does not pause a running gateway. Normal users change the folder through the settings window, which owns that coordination.
