import { dirname, join, relative, resolve, sep } from 'node:path';
import { realpath } from 'node:fs/promises';
import { isModelCredentialPath, within } from './tool-paths.mjs';
import { extensionControlPaths, isExtensionControlPath, isExtensionManagedPath } from './extension-storage.mjs';

/** Storage identity and protected-path policy; never grants authority to execute an operation. */
export class ToolStorageBoundary {
  constructor({ root, dataHome, extensionRoot, extensionPointer, officialToolsRoot }) {
    this.root = root;
    this.dataHome = dataHome;
    this.extensionRoot = extensionRoot;
    this.extensionPointer = extensionPointer;
    this.officialToolsRoot = officialToolsRoot;
    this.roots = [...new Set([root, dataHome, extensionRoot, officialToolsRoot,
      ...extensionControlPaths(extensionPointer).map(path => dirname(path))])];
    this.storageAliases = this.roots.map(path => [path, path]);
  }

  async refresh() {
    // Publish a complete snapshot only after all canonical roots have been resolved.
    this.storageAliases = await Promise.all(this.roots.map(async root => {
      try { return [root, await realpath(root)]; }
      catch (error) {
        if (error.code !== 'ENOENT') throw error;
        return [root, root];
      }
    }));
  }

  aliases(path) {
    const paths = new Set([resolve(path)]);
    for (const [lexical, canonical] of this.storageAliases) {
      if (within(lexical, path)) paths.add(resolve(canonical, relative(lexical, path)));
      if (within(canonical, path)) paths.add(resolve(lexical, relative(canonical, path)));
    }
    return [...paths];
  }

  isCredential(path) {
    return this.aliases(path).some(alias => isModelCredentialPath(alias, this.dataHome, this.root) ||
      isModelCredentialPath(alias, this.dataHome, this.extensionRoot) ||
      within(join(this.extensionRoot, 'Backups', 'Extensions'), alias));
  }

  isOwned(path) {
    return this.aliases(path).some(alias => [this.root, this.dataHome, this.extensionRoot].some(root => within(root, alias)));
  }

  isManagedExtension(path) {
    return this.aliases(path).some(alias => isExtensionManagedPath(alias, this.extensionRoot));
  }

  isReadOnlyExtension(path) {
    return this.aliases(path).some(alias => isExtensionControlPath(alias, this.extensionPointer) ||
      within(this.officialToolsRoot, alias) || isExtensionManagedPath(alias, this.extensionRoot));
  }

  isPrivateResult(path) {
    return this.aliases(path).some(alias => within(this.root, alias) &&
      !within(join(this.root, 'Desktop', 'Projects'), alias) && !within(join(this.dataHome, 'Workspaces'), alias) &&
      relative(this.root, alias).split(sep).some(part => part.toLowerCase() === 'tool-results'));
  }
}
