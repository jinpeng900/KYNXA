# Source and adaptation

- Source: https://github.com/anthropics/skills/tree/8a1541c4a3ffa5a20a5a91de0dcf3f0bab1d1ef4/skills/internal-comms
- Upstream commit: `8a1541c4a3ffa5a20a5a91de0dcf3f0bab1d1ef4`.
- Original `SKILL.md` SHA-256: `c5c061ab25b33fab2358a9be53a5cc0d528ec7c3e8a29285a041ffc95d0d8947`.
- License: Apache-2.0; complete upstream license is preserved in `LICENSE.txt`.
- Changes: use a model-independent assistant name and add provenance metadata. Example resources are copied unchanged.

This is a writing workflow, with no executable dependency or separate filesystem implementation. In KYNXA, load the selected `examples/*.md` with `skill.resource.read` using this skill's discovered ID. Read and save work files through the existing filesystem tools, following ordinary approvals and conflict checks. It does not grant permission to contact other people or services.
