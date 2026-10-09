# Mermaid browser runtime

- Version: **12.1.0** (pinned; no runtime CDN or network download).
- Package: https://registry.npmjs.org/mermaid/-/mermaid-12.1.0.tgz
- Official project: https://github.com/mermaid-js/mermaid
- Package integrity: `sha512-wlVCp+8eTupfCeeFvoZNNiTuHrvag0P2jz/ILgb/f/6jkVokUefOcujefi8qUe/j2asHiSePncVsz/xzzA80LQ==`
- `mermaid.min.js` SHA-256: `6484afc32872a3aa16cac9a76ba1816a1ed4cc870a6593cc2e17757750f518b2`

`mermaid.min.js` is the unmodified complete classic browser bundle from
`package/dist/mermaid.min.js`. Its third-party license notices remain embedded.
`LICENSE` is the original package MIT license. The archive's SHA-512 was checked
against npm registry metadata before extraction.

The transcript loads this runtime only in an iframe with `sandbox="allow-scripts"`,
without same-origin, network, popup, form, download or navigation permissions.
Application code does not call Mermaid's interaction binding functions. The
parent validates and sanitizes the returned SVG before adding it to the chat.

Update this directory deliberately, verify the archive integrity and bundled
notices, and rerun `tests/transcript-markdown-smoke` and the real WebView2 diagram
checks in `tests/transcript-ui-smoke` after changing the version.
