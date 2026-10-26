---
name: blender-visual-check
description: Refine Blender scenes through the configured MCP, inspect fresh delivered images, compare revisions and verify visual quality. 用 Blender MCP 精修三维建模、材质、灯光与渲染，真实看图并对比验收。
license: Apache-2.0
compatibility: An enabled, authorized Blender MCP with scene and image tools; native desktop tools are optional. No bundled scripts or additional dependencies.
metadata:
  capability: blender
  runtime: native
---

# Blender visual check

Use this workflow for Blender modeling, materials, lighting and rendered presentation. Discover the configured Blender MCP with `tool.search`, then load the actual advertised tools with `tool.load`. Server IDs and tool names vary; do not invent a tool or assume an installed MCP is connected. Load `desktop-workflow` only when application launch, window identification or native screenshots are needed.

Follow the user's current stage: a request first to explain the plan ends with that explanation, without beginning its future modeling or rendering. A requested plan file may itself be saved. Choose ordinary design details yourself; ask only about a critical missing fact, material ambiguity or permission. A confirmed unsupported necessary capability or contradictory target should be explained with its basis and reasonable alternatives, not retried indefinitely; one ineffective modeling technique is not proof of impossibility.

## Establish the current scene and target

- Inspect the live Blender scene, active file, engine and relevant objects through MCP before editing. Work within the user's authorized project and files; do not inspect unrelated projects for examples. Reuse the current scene rather than resetting or reopening it without a reason.
- Keep a short visual checklist of the user's requested qualities and features; do not add criteria for an ordinary launch, scene query or save. A reference is guidance, not proof that those features exist in the scene.
- Use the requested renderer and available hardware. Verify the engine, GPU selection or Eevee ray-tracing setting through current scene state rather than assuming a switch took effect. Save a recoverable milestone before substantial structural changes.

## Make changes and obtain useful views

1. Make a coherent group of edits with the available scene/code/material tools. Keep object identity and successful work where possible. A node connection proves configuration, not that a normal map or ornament is visibly effective.
2. When a change affects a requested visual criterion, produce a useful viewport image or render that tests it: an overall view for composition or a close-up for an unresolved detail. Reuse unchanged, previously inspected images. Do not render after every minor edit or demand fixed image counts; scene/configuration/save facts can use trusted MCP receipts.
3. Compare against the last image actually inspected, preferably with comparable framing, scale, engine and exposure. If those changed, distinguish that effect from a modeling improvement. Inspect relevant pixels for clipping, intersections, black seams, washed-out colors, missing detail and the requested atmosphere; do not assert their absence solely from a successful render or object inventory.
4. Correct the visible mismatch, then inspect a new image of that correction. Change technique when repeated attempts produce the same defect; a simpler successful representation is preferable to repeating an ineffective operation. Retain the rest of the completed scene.

## Image delivery and acceptance are separate

An archived PNG, path, byte count, hash, dimensions, screenshot receipt or preview registration does not establish that the current model received its pixels. Claim to have seen an image only after actual image content was delivered to the model. Vision-capable routes can receive real tool images; do not assume all models are text-only.

When only metadata arrived, use `image.read` to obtain the saved local image pixels, not `artifact.open` or another capture of the unchanged scene; do not ask the user to reupload a known local image. If the current model does not support vision, state that limitation without inventing a secondary model route or visual observations. Scene inspection can still establish geometry, configuration and file facts.

Keep capture and analysis in the background. Native screenshots default to temporary storage; `question` can identify the current visual issue. Permanent `retention` requires a nonempty `retentionReason` for user-requested preservation or key evidence; later use `image.retain(resultId,reason)` on an existing result. Retention never opens or selects the right workspace. Only explicitly requested images or selected final deliverables belong there. For a useful summary or key result, use `artifact.open(resultId)` to display the saved image without recapturing; avoid repeated workspace switches during execution.

Image delivery itself does not mean quality passed. Accept each requested visual criterion only when the inspected result supports it. A render completing, a material being assigned or a file being saved is insufficient evidence of beauty, recognizable sculpture, rich color or convincing surface detail.

## Assets, continuity and delivery

- Prefer assets available through the user's enabled free libraries or authorized online resources. Check the individual asset's license and reuse conditions, retain attribution/source information when required, and do not assume that a free download permits every use. Do not start paid generation or activate a paid plan by default; use it only within the user's authorization. A configured API key alone does not authorize new charges.
- For resource pressure, retain the scene, saved path, completed changes, inspected views and outstanding checks; let the existing resource scheduler queue work. Use a lower-cost useful preview when appropriate. Waiting or pausing is not task completion and must not erase progress.
- After an unknown save, import, launch or other side-effect result, inspect the same target before retrying. Reuse verified successes; do not recreate the scene or duplicate assets simply because context was compressed or the task resumed.
- Keep progress brief. Continue feasible criteria that remain in the current authorized scope; optional refinements are not completion gates. Reuse adequate inspected views rather than padding time with waits or redundant renders. A requested explanation, plan, stop or deliver-existing instruction changes the current deliverable and takes priority over earlier refinement goals.
- Save the final editable project and useful requested renders, verify their actual save receipts and present selected final outputs. Reuse existing preview tabs for revisions instead of opening every intermediate render. Report any genuinely unresolved criterion concisely without labeling it as completed.

This is an original KYNXA workflow. It provides no additional permissions, execution ledger or global memory; scene evidence and checkpoints remain scoped to the current task.
