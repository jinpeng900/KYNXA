---
name: safe-file-edit
description: Make targeted, conflict-aware changes to mounted work files and verify their observable result through approved tools.
---

# Make a targeted file change

Use this skill when the user requests creating, modifying or deleting a work file.

1. Read the relevant existing files and locate the owner of the requested behavior. Keep unrelated user changes.
2. Before replacing or deleting a file, get its SHA-256 from `filesystem.read` or `filesystem.stat`. Supply that exact `expectedHash`; for a new file supply `expectedHash: null`.
3. Prefer `filesystem.edit` for one precise literal replacement. Use `filesystem.write` for a new file or a deliberate complete replacement. A conflict means re-read and reconsider; never overwrite blindly.
4. Use `filesystem.delete` only for an explicitly requested file or empty directory. Do not simulate recursive deletion with a loop.
5. Verify with scoped read/search tools. If a suitable command is supported, `terminal.run` executes it in a temporary AppContainer work copy. Its modifications are not applied to the real work folder automatically.
6. Report the changed paths, what was checked and any failure. A refused or unavailable tool did not execute.

Every call uses the application's current request permissions. This skill cannot bypass approval or turn off the terminal sandbox.
