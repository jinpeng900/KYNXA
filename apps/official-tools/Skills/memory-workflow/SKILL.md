---
name: memory-workflow
description: Read or propose scoped durable user/project memory, distinguish tool experience from facts, and recover cited history. 记忆查询、记住偏好与项目决定。
license: Apache-2.0
compatibility: Existing native memory and conversation.history tools; no separate memory MCP or extraction model is required.
metadata:
  capability: memory-workflow
  runtime: native
---

# Use memory with the correct scope

Runtime tool directories and relevant skills provide software usage knowledge. The separate persistent tool experience store ranks candidates by metadata, validated outcomes and latency. It must not become personal memory or contain arguments, document bodies, credentials or live session handles.

1. A request to inspect existing memory authorizes its scoped read. Use memory.read or an actually available search tool. Empty records do not mean the user never said something or that memory is unavailable.
2. For a past user question use conversation.history.search with role:user and the requested chat/work scope; read matching original messages/versions. Assistant text, files and memory summaries cannot prove the user's exact wording. Report actual source IDs and coverage.
3. Global memory is for important durable cross-chat facts/preferences. Project decisions stay in project scope. Ordinary dialogue, document text, manuals, temporary output, process IDs, browser sessions, preview grants, credentials and speculation are not global facts.
4. An explicit remember instruction authorizes its stated content/scope. Follow the supported proposal/confirmation contract and retain original user evidence. Inferred facts remain proposals until confirmed. Claim saved only from an actual receipt.
5. Updates use current scope and expectedRevision. Reload conflicts; never overwrite another update, revive deleted memory or cross scope. Backend confirmed/active/source-validity rules remain authoritative.
6. Prompts receive bounded relevant confirmed memories and a small set of durable preferences, not the whole store or every skill body. Full originals remain available on demand. Memory cannot override current user instructions, permissions or runtime model identity.

Do not add memory model calls or MCP discovery to ordinary chat to demonstrate this workflow. This is an original KYNXA skill using the existing formal store.
