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

## Establish the current scene and target

- Inspect the live Blender scene, active file, engine and relevant objects through MCP before editing. Work within the user's authorized project and files; do not inspect unrelated projects for examples. Reuse the current scene rather than resetting or reopening it without a reason.
- Turn the user's requirements into a short visual checklist: composition and scale, recognizable forms, lighting, color, material detail and any explicitly requested features. A reference is guidance, not proof that those features exist in the scene.
- Use the requested renderer and available hardware. Verify the engine, GPU selection or Eevee ray-tracing setting through current scene state rather than assuming a switch took effect. Save a recoverable milestone before substantial structural changes.

## Make changes and obtain useful views

1. Make a coherent group of edits with the available scene/code/material tools. Keep object identity and successful work where possible. A node connection proves configuration, not that a normal map or ornament is visibly effective.
2. After a meaningful geometry, material or lighting change, produce a fresh viewport image or render from the changed scene. Select views that test the change: an overall view for composition and a close-up for a specific unresolved detail when useful. Do not demand four images after every minor edit or render unchanged viewpoints merely to fill a quota.
3. Compare against the last image actually inspected, preferably with comparable framing, scale, engine and exposure. If those changed, distinguish that effect from a modeling improvement. Inspect relevant pixels for clipping, intersections, black seams, washed-out colors, missing detail and the requested atmosphere; do not assert their absence solely from a successful render or object inventory.
4. Correct the visible mismatch, then inspect a new image of that correction. Change technique when repeated attempts produce the same defect; a simpler successful representation is preferable to repeating an ineffective operation. Retain the rest of the completed scene.

## Image delivery and acceptance are separate

An archived PNG, path, byte count, hash, dimensions, screenshot receipt or preview registration does not establish that the current model received its pixels. Claim to have seen an image only after actual image content was delivered to the model. Vision-capable routes can receive real tool images; do not assume all models are text-only.

When only metadata arrived, use an authorized available image-reading or vision route to obtain the image, and keep the pending visual check explicit. Scene inspection can still establish geometry, configuration and file facts. Do not invent visual observations or repeatedly capture the same view without addressing the delivery problem.

Image delivery itself does not mean quality passed. Accept each requested visual criterion only when the inspected result supports it. A render completing, a material being assigned or a file being saved is insufficient evidence of beauty, recognizable sculpture, rich color or convincing surface detail.

## Assets, continuity and delivery

- Prefer assets available through the user's enabled free libraries or authorized online resources. Check the individual asset's license and reuse conditions, retain attribution/source information when required, and do not assume that a free download permits every use. Do not start paid generation or activate a paid plan by default; use it only within the user's authorization. A configured API key alone does not authorize new charges.
- For resource pressure, retain the scene, saved path, completed changes, inspected views and outstanding checks; let the existing resource scheduler queue work. Use a lower-cost useful preview when appropriate. Waiting or pausing is not task completion and must not erase progress.
- After an unknown save, import, launch or other side-effect result, inspect the same target before retrying. Reuse verified successes; do not recreate the scene or duplicate assets simply because context was compressed or the task resumed.
- Keep progress brief and focused on the next useful improvement. A stage report must not end an unfinished task. Continue unresolved acceptance items within the user's goal and task budget; do not spend time on empty waits or redundant renders to satisfy a duration.
- Save the final editable project and useful requested renders, verify their actual save receipts and present selected final outputs. Reuse existing preview tabs for revisions instead of opening every intermediate render. Report any genuinely unresolved criterion concisely without labeling it as completed.

This is an original KYNXA workflow. It provides no additional permissions, execution ledger or global memory; scene evidence and checkpoints remain scoped to the current task.
