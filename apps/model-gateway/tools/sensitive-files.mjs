const credentialFiles = new Set(['.npmrc', '.pypirc', '.netrc', '.git-credentials',
  'credentials', 'credentials.json', 'secrets.json', 'secrets.yaml', 'secrets.yml',
  'application_default_credentials.json', 'accesstokens.json']);
const credentialDirectories = new Set(['.ssh', '.gnupg']);

// Classify paths before opening files; content inspection would already expose the secret.
// 在打开文件前按路径分类；先读取内容再判断会越过需要审批的边界。
export function isSensitiveFilePath(path) {
  const components = String(path).toLowerCase().split(/[\\/]/).filter(Boolean);
  const filename = components.at(-1) ?? '';
  return filename === '.env' || filename.startsWith('.env.') || credentialFiles.has(filename) ||
    components.some(component => credentialDirectories.has(component)) ||
    /^(?:id_(?:rsa|dsa|ecdsa|ed25519))(?:\.|$)/.test(filename) ||
    /\.(?:p12|pfx|key)$/.test(filename);
}
