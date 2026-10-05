/**
 * A protocol-1 terminal helper may ignore skill fields; skill execution needs an explicit capability and receipt.
 * 协议版本 1 的终端助手可能忽略技能字段，执行技能必须取得明确能力声明与回执。
 */
export function supportsSkillExecution(capabilities) {
  const proof = capabilities?.skillExecution;
  return proof?.manifestVersion === 1 && proof.readOnlyPackage === true && proof.hashChecked === true;
}

export function verifiesSkillExecution(result, prepared) {
  const proof = result?.skillExecution;
  const script = prepared?.files?.find(file => file.relativePath === prepared.scriptRelativePath);
  return supportsSkillExecution(result) && script && proof.script === prepared.scriptRelativePath &&
    proof.fileCount === prepared.files.length && proof.scriptSha256 === script.sha256;
}
