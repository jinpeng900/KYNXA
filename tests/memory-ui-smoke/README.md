# Native memory management UI smoke

This isolated WinUI host links the production memory window, view model, contracts,
localization, and styles. It uses an in-memory `IMemoryApi` fixture and sets a unique
temporary `KYNXA_DATA_HOME` before loading application components. No real gateway,
credentials, chat history, or application Data directory is used.

Run from the repository root after building:

```powershell
dotnet run --project tests/memory-ui-smoke/MemoryUiSmoke.csproj
```

The host exercises real native controls through automation peers and programmatic
control input. It does not claim physical mouse or keyboard interaction. Native
dialog buttons are invoked through their exposed automation peers. Window resize,
minimize, and restore use the real AppWindow presenter. Screenshots use Windows
`PrintWindow` and direct PNG encoding without modifying the captured pixels.

Multiline WinUI TextBoxes expose the Text pattern rather than writable Value;
the host sets the actual control's `Text` to exercise its production `TextChanged`
handler, using the native CR newline representation. ComboBox selections use the
real control's selection property and event. Close-confirmation checks post native
`WM_CLOSE`, because `Window.Close()` directly closes a window without raising an
`AppWindow.Closing` request. Dialog checks wait for `ContentDialog.Closed`, including
the closing animation, before invoking the next action.

Coverage includes scope isolation, search, editing and creation, delete/discard
confirmations, archived and unavailable source states, archived-project CRUD,
empty/error states, a 409 conflict preserving input, late results during rapid
scope/context changes and after close, live Chinese/English switching, and native
window resizing at 600/960/1600 pixels plus minimize/maximize/restore.

Results and previews are written beneath `%TEMP%/kynxa-memory-ui-smoke-*`. The
latest result location is recorded in `%TEMP%/kynxa-memory-ui-smoke-latest.txt`.
