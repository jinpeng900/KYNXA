# Native sandbox smoke

On Windows, run from the repository root:

```powershell
dotnet build apps/tool-host/KYNXA.ToolHost.csproj
node --test tests/sandbox-smoke/sandbox-smoke.test.mjs
```

This starts actual AppContainer Node and cmd processes with fake, temporary fixture files. It checks snapshot writes without modifying the source, Unicode/quoted argv and isolated environment, exclusion of fake credentials, junctions and hard links, an OS-denied external temporary file read, a denied loopback HTTP request with no request arriving at the host fixture server, cmd echo/type/redirection and a cmd-created network-denied Node child, a snapshot Node test, real descendant termination after normal exit/timeout/cancellation (including cmd → Node → child), bounded output, an OS-denied 400 MiB allocation, trusted canonical managed-work copying while Data/Chats is rejected, and refusal of unsupported commands, arbitrary executable paths, debugger ports, a missing helper and cleanup of an unrelated directory. cmd DIR compatibility is reported separately; a denied volume query does not count as successful directory listing. Cancellation waits for a native start record and a child-created readiness file before aborting; process existence checks use the actual PIDs. Fixtures and completed stages are removed afterward. No models, real credentials, chats or memory data are used.

To verify the packaged self-contained helper, set `KYNXA_SANDBOX_SMOKE_TOOL_HOST` to its absolute `KYNXA.ToolHost.exe` path before running the same smoke. Every positive fixture then uses that helper; the missing-helper negative test remains separate.

Non-Windows machines skip the native smoke. A skip is not evidence of isolation. This validates the tested Windows/Node/runtime combination; other architecture builds require their own native run. `capabilities()` reports a supported helper protocol, not a claim that every possible script or third-party native module is compatible.
