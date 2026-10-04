---
name: safe-file-edit
description: Create directories and files, make targeted conflict-aware edits or deletions, and verify their result in the current linked or isolated work folder.
license: Apache-2.0
compatibility: Uses the installed filesystem tools; optional tests use the verified sandbox or explicitly approved host terminal. No Python is required.
---

# Make a targeted file change

Use this skill when the user requests creating, modifying or deleting a work file or directory. It works in the current linked folder or an unlinked conversation's isolated persistent folder.

1. Read the relevant existing files and locate the owner of the requested behavior. Keep unrelated user changes.
2. Before replacing or deleting a regular file, get its SHA-256 from `filesystem.read` or `filesystem.stat`. Supply that exact `expectedHash`; for a new file supply `expectedHash: null`. For a long readable file, collect pages using each returned `nextOffset` until `hasMore` is false, checking that the full-file hash remains identical on every page. A changed hash means discard those pages and read the new version. `truncated` can still be true on the last page because it covers only part of the file; it is not the paging termination condition. Do not completely replace a file from one preview. Files above the supported size and non-UTF-8 files cannot be safely rewritten through these text tools.
3. Prefer `filesystem.edit` for one precise literal replacement. Use `filesystem.write` for a new file or a deliberate complete replacement. A conflict or non-unique replacement means re-read and reconsider; never overwrite blindly.
4. A file's parent must already exist. Use `filesystem.mkdir` for one requested directory whose parent exists; it keeps an existing directory. For a requested nested structure, create missing parents in dependency order and verify each receipt.
5. Use `filesystem.delete` only for an explicitly requested regular file with its exact hash, or an empty directory with `expectedHash: null`. It does not recursively delete or remove the work root. Do not simulate recursive deletion with a loop or a host command.
6. Verify with scoped read/search tools. If a suitable command is supported, `terminal.run` executes it in a temporary AppContainer work copy. Its modifications are not applied to the real work folder automatically. Use terminal-workflow for an explicitly needed host-dependent test rather than claiming that the sandbox can run the user's installed tools.
7. Access outside the current folder needs a concrete task reason and the application's approval decision. Links and protected application data cannot be bypassed by choosing another path or command.
8. Report the changed paths, what was checked and any failure. A refused tool did not execute; an interrupted or unknown outcome needs an observation before retrying a change.

Every call uses the application's current request permissions. This skill cannot bypass approval or turn off the terminal sandbox.
