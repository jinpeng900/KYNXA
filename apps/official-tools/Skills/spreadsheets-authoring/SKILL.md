---
name: spreadsheets-authoring
description: Generate real XLSX Excel workbooks with typed cells and formulas, or CSV/TSV exports, and check the saved data. 创建电子表格、工作簿和数据表。
license: Apache-2.0
compatibility: Native document.create/read and filesystem.write; actual formula evaluation needs an available calculation tool.
metadata:
  capability: spreadsheets-authoring
  runtime: native
---

# Produce a trustworthy workbook

Reuse unchanged Skill instructions and loaded tools; load known capabilities needed for the current stage together with tool.load(names). Independent workbooks may be read together. A conversation-only follow-up can be answered directly without generating, restating or reopening a workbook.

1. Identify actual columns, units, missing values and calculation requirements. Keep identifiers with leading zeroes as strings. Numeric and Boolean values retain their types; unknown is not zero.
2. For XLSX use document.create with path and sheets: [{"name":"Results","rows":[["Item","Quantity","Price","Total"],["A",2,3.5,{"formula":"B2*C2","cachedValue":7}]]}]. A formula is an explicit object, not an ordinary string beginning with "=". Supply cachedValue only when actually computed and checked. The generator stores formulas and requests recalculation on opening; it does not calculate them.
3. For CSV/TSV use filesystem.write with correct delimiter quoting and escaping. Untrusted strings beginning with formula characters must not silently become formulas. Renamed CSV is not XLSX.
4. Stat an existing target and pass its exact expectedHash; null expects a new file. Read the saved workbook with document.read and check sheet names, row/column coverage and representative data. Do not claim recalculation when only expressions or caches were written.
5. Charts, pivots, macros, arbitrary template editing and actual formula evaluation require an appropriate available tool. Preserve original files and report the specific limitation instead of silently flattening them.
6. Use artifact.open for the final requested workbook and reuse its path/tab on revisions. Ordinary XLSX previews directly in the right workspace with worksheet switching; displayed formula values are the file's stored caches, not newly calculated results. A registered/pending receipt does not establish rendering or failure and is not a reason to request external Office. Actual unsupported charts, pivots, embedded objects or a real render failure use the same version's cached PDF when conversion is available. Direct preview is not complete Excel compatibility or formula recalculation. Existing views retain their state; refresh only when needed. Data rows and temporary analysis are not global personal memory.

For required complete readback, follow the current file hash and fill the actual unread ranges, including the last partial page. Rewrites through terminal or external tools invalidate old coverage. task.document records conclusions and does not replace a saved-file read. Reuse unchanged previews and inspect affected sheets or ranges after a revision; do not repeat screenshots or acceptance without new evidence.

Original KYNXA workflow. Public references: [ExcelJS](https://github.com/exceljs/exceljs), [Open XML SDK](https://github.com/dotnet/Open-XML-SDK).
