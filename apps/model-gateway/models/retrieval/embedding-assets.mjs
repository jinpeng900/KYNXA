import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { lstat } from 'node:fs/promises';
import { join } from 'node:path';
import { BUILTIN_EMBEDDING_PROFILE } from './embedding-profile.mjs';

export async function verifyEmbeddingAsset(modelRoot, asset) {
  const assetPath = join(modelRoot, asset.path);
  const file = await lstat(assetPath);
  if (!file.isFile() || file.isSymbolicLink() || file.size !== asset.bytes) return false;
  const digest = createHash('sha256');
  for await (const part of createReadStream(assetPath)) digest.update(part);
  return digest.digest('hex') === asset.sha256;
}

export async function verifyEmbeddingBundle(modelRoot) {
  for (const asset of BUILTIN_EMBEDDING_PROFILE.files) {
    if (!await verifyEmbeddingAsset(modelRoot, asset)) {
      const error = new Error('Bundled embedding assets failed integrity verification.');
      error.code = 'EMBEDDING_ASSET_INVALID';
      throw error;
    }
  }
}
