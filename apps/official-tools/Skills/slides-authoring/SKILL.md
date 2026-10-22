---
name: slides-authoring
description: Create editable PPTX PowerPoint slides with concise titles, text, bullets and tables, then verify contents. 制作 PPT、幻灯片和演示文稿。
license: Apache-2.0
compatibility: Native document.create and document.read; no PowerPoint or separate script runtime is required.
metadata:
  capability: slides-authoring
  runtime: native
---

# Build an editable slide deck

Reuse unchanged Skill instructions and loaded tools; batch known capabilities needed for this stage with tool.load(names). Independent files may be read together. Summarizing an existing conversation does not require regenerating a deck or opening its preview again.

1. Read the actual sources and choose a clear sequence for the intended audience. One main point per slide; keep text readable and split overfull slides. Do not invent figures or turn a long report into one slide.
2. Use document.create with a .pptx path, optional title and slides: [{"title":"Main finding","body":"Short explanation","bullets":["Verified point","Next action"]},{"title":"Results","rows":[["Measure","Value"],["Checked","Yes"]]}]. An existing target requires its current expectedHash; a new file uses null.
3. The native package contains editable slides, layout, master and theme. Read it back with document.read and check slide count/order, numerical data and missing content. The right workspace previews ordinary PPTX text, shapes, images and tables using slide coordinates, with previous/next, page number, keyboard and zoom controls. A validated package is not proof that every line fits; inspect the actual preview when layout matters.
4. Native generation supports titles, text, bullets and tables. Arbitrary template editing, advanced charts, embedded video and exact layout need a compatible enabled tool. Preserve an original deck and never rename Markdown to PPTX.
5. Open the final deck with artifact.open, reuse its path/tab for revisions and provide the actual file link. Registration/pending metadata is not a rendered page or a failure; never infer that the user needs external Office from that receipt. Actual unsupported charts, SmartArt, embedded objects or a real render failure use the same version's cached PDF when conversion is available. Static preview is not full Office or animation compatibility. Rendered pages, current page and zoom are retained; refresh only when needed. Intermediate sources need not open automatically. Slide content is task data, not global personal memory.

Follow the current file hash and returned unread ranges until the required readback is complete. Revisions made by terminal or external tools invalidate the old version's coverage. A task.document analysis entry is not a read receipt. Reuse unchanged slide previews, inspect affected slides after changes, and keep page switching before its screenshot; do not repeatedly open the same version to fill a review cycle.

Original KYNXA instructions. Public references: [PptxGenJS](https://github.com/gitbrent/PptxGenJS), [Open XML SDK](https://github.com/dotnet/Open-XML-SDK).
