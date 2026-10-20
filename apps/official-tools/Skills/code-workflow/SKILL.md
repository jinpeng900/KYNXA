---
name: code-workflow
description: Locate code and callers, make scoped conflict-aware edits, and run relevant checks without waiting for indexes. 代码检索、编程、修复与重构。
license: Apache-2.0
compatibility: Existing native filesystem and knowledge tools, with optional authorized sandbox or host terminal checks.
metadata:
  capability: code-workflow
  runtime: native
---

# Complete a scoped code change

1. Read repository instructions and identify the requested behavior. Use work.folder.list when mount identity matters; same-name files across roots differ. Mounting folders does not authorize their shared parent. Preserve unrelated changes.
2. Start from exact paths, symbols and error codes with scoped filesystem.search/read. Follow page/hash boundaries. Use existing knowledge evidence for semantic questions; lexical retrieval and direct reads remain usable while vectors or ANN prepare in the background.
3. Verify definitions, callers, configuration and relevant tests. Retrieval excerpts and codeNavigation cover selected evidence, not a complete dependency graph. Current source is authoritative.
4. Use filesystem.edit/write with the current expectedHash. On conflict re-read. Keep contracts and existing behavior unless requested otherwise; binary documents use their own tools.
5. Run the smallest relevant checks. For a requested visible terminal use terminal.workspace.list/open/run and its actual commandId/exitCode. Otherwise use an authorized compatible sandbox/host tool. Accepted input is not completion; wait expiry is not cancellation. Do not interrupt the user's busy terminal.
6. Inspect the diff and real results. Continue from saved progress through pauses, compression and guidance. Observe unknown effects before retrying, never replay successful operations. Deliver the changed behavior, actual checks, limits and useful file links.

A small edit needs no report/task bookkeeping. Project decisions use project memory only with user authorization; source code, tool manuals and test output do not become global facts. This original workflow grants no new permissions.
