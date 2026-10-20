# DOCX preview dependencies

These browser bundles are shipped with KYNXA. They are loaded only from the local workspace assets; no CDN, npm install, or runtime download is used.

| Package | Fixed version | Source | License selected |
|---|---|---|---|
| docx-preview | 0.4.1 | https://registry.npmjs.org/docx-preview/-/docx-preview-0.4.1.tgz · https://github.com/VolodymyrBaydalka/docxjs | Apache-2.0 |
| JSZip | 3.10.2 | https://registry.npmjs.org/jszip/-/jszip-3.10.2.tgz · https://github.com/Stuk/jszip | MIT (the upstream dual-license text is retained unchanged) |

The original license files and bundle notices are retained alongside the distributions. The JSZip browser bundle includes pako and its MIT notice. Third-party bundles are copied unchanged.

KYNXA disables DOCX altChunks/comments/experimental code. The trusted parent renderer writes the document into a sandboxed frame without script permission. The frame permits only embedded blob/data images and fonts, denies network requests/forms/objects, and cannot expose a native bridge through document code. Native content is fetched once through the dedicated `https://kynxa-workspace-content.local/Workspace/document.docx` proxy, which allows the static workspace origin only. Late rendering results cannot replace another selected document; native tab caching retains completed DOMs.
