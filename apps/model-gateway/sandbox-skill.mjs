/** A protocol-1 terminal helper may ignore skill fields; skill execution needs an explicit capability and receipt. */
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
