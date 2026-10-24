---
name: safe-file-edit
description: Create directories and files, make targeted conflict-aware edits or deletions, and verify results in the work folder or at an explicitly authorized external target.
license: Apache-2.0
compatibility: Uses the installed filesystem tools; optional tests use the verified sandbox or explicitly approved host terminal. No Python is required.
---

# Make a targeted file change

Use this skill when the user requests creating, modifying or deleting a file or directory. The linked or isolated persistent work folder is the default; an external target explicitly authorized by the user is also in scope under the current permission policy.

1. Read the relevant existing files and locate the owner of the requested behavior. Keep unrelated user changes.
2. Before replacing or deleting a regular file, get its SHA-256 from `filesystem.read` or `filesystem.stat`. Supply that exact `expectedHash`; for a new file supply `expectedHash: null`. For a long readable file, collect pages using each returned `nextOffset` until `hasMore` is false, checking that the full-file hash remains identical on every page. A changed hash means discard those pages and read the new version. `truncated` can still be true on the last page because it covers only part of the file; it is not the paging termination condition. Do not completely replace a file from one preview. Files above the supported size and non-UTF-8 files cannot be safely rewritten through these text tools.
3. Prefer `filesystem.edit` for one precise literal replacement. Use `filesystem.write` for a new UTF-8 file or a deliberate complete replacement. For a genuine DOCX/PDF/PPTX/XLSX use `document.create` and the related authoring skill; a changed extension does not convert text into a document. A conflict or non-unique replacement means re-read and reconsider; never overwrite blindly.
4. A file's parent must already exist. Use `filesystem.mkdir` for one requested directory whose parent exists; it keeps an existing directory. For a requested nested structure, create missing parents in dependency order and verify each receipt.
5. `filesystem.delete` supports an explicitly requested regular file with its exact hash, or an empty directory with `expectedHash: null`; this tool does not recursively delete or remove the work root. For a directory tree the user explicitly asks to delete, use `terminal.host.run` under the current permission policy. Resolve and inspect the exact target first, then execute the bounded deletion command; do not replace the request with endless inventory or refuse merely because the target is outside the work folder. This does not permit bypassing a real policy denial or changing neighboring paths.
6. Verify with scoped read/search tools. If a suitable command is supported, `terminal.run` executes it in a temporary AppContainer work copy. Its modifications are not applied to the real work folder automatically. Use terminal-workflow for an explicitly needed host-dependent test rather than claiming that the sandbox can run the user's installed tools.
7. Access outside the current folder needs a concrete task reason and follows current application permissions. Full mode needs no per-call approval; Ask/Smart use the existing approval flow. Do not ask again merely because the authorized target is external. Links and protected application data cannot be bypassed by choosing another path or command. An earlier model refusal is not a current runtime rule; apply the user's latest authorization and actual tool policy.
8. Report the changed paths, what was checked and any failure. A refused tool did not execute; an interrupted or unknown outcome needs an observation before retrying a change.

Every call uses the application's current request permissions. This skill cannot bypass approval or turn off the terminal sandbox.
