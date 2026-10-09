import { basename } from 'node:path';
import { readSkillResource, skillScriptRuntime } from './skill-resources.mjs';
import { supportsSkillExecution } from './sandbox-skill.mjs';

const diagnostic = (code, message, severity = 'warning') => ({ code, message, severity });

function checkNodeVersion(requirement) {
  const terms = requirement.trim().split(/\s+/);
  const current = process.versions.node.split('.').map(Number);
  if (!terms.length || terms.some(term => !/^(?:>=|<=|>|<|=)?\d+(?:\.\d+){0,2}$/.test(term))) return null;
  return terms.every(term => {
    const [, operator = '=', value] = /^(>=|<=|>|<|=)?(\d+(?:\.\d+){0,2})$/.exec(term);
    const expected = value.split('.').map(Number);
    if (operator === '=') return expected.every((part, index) => current[index] === part);
    const partial = expected.length < 3;
    // Partial inclusive upper ranges (<=22, >22.1) cover their entire major/minor series.
    // 带部分版本的比较范围按主版本或次版本系列处理，例如 <=22 和 >22.1。
    if (partial && ['<=', '>'].includes(operator)) expected[expected.length - 1]++;
    while (expected.length < 3) expected.push(0);
    let comparison = 0;
    for (let i = 0; i < 3; i++) if (current[i] !== expected[i]) { comparison = current[i] > expected[i] ? 1 : -1; break; }
    return operator === '>=' ? comparison >= 0 : operator === '<=' ? (partial ? comparison < 0 : comparison <= 0) :
      operator === '>' ? (partial ? comparison >= 0 : comparison > 0) : comparison < 0;
  });
}

/**
 * Describe requirements using declared sandbox capabilities; never probe with host commands.
 * 仅根据已声明沙箱能力描述要求，不通过宿主命令探测。
 */
export async function checkSkillEnvironment(skill, inventory, { sandboxCapabilities, signal, scriptPath, denyResource } = {}) {
  const capabilities = sandboxCapabilities ?? {}, commands = Array.isArray(capabilities.commands)
    ? [...new Set(capabilities.commands.filter(value => typeof value === 'string').map(value => value.replace(/\.exe$/i, '').toLowerCase()))] : [];
  const diagnostics = [...inventory.diagnostics], dependencies = [];
  const skillSandbox = supportsSkillExecution(capabilities);
  const scripts = inventory.files.filter(file => (file.relativePath.startsWith('scripts/') || !file.relativePath.includes('/')) &&
    /\.(?:mjs|cjs|js|py|ps1|sh|bash|cmd|bat|rb|pl|lua|php)$/i.test(file.relativePath)).map(file => {
    const runtime = skillScriptRuntime(file.relativePath);
    return { path: file.relativePath, runtime,
      supported: runtime === 'node' && capabilities.available === true && commands.includes('node') && skillSandbox };
  });
  if (scripts.some(script => script.runtime === 'node') && !skillSandbox)
    diagnostics.push(diagnostic('SKILL_SANDBOX_UNSUPPORTED', 'The verified helper does not advertise manifest-v1, hash-checked, read-only skill package execution.', 'error'));
  for (const script of scripts) if (!script.supported)
    diagnostics.push(diagnostic('SKILL_RUNTIME_UNAVAILABLE', `Script ${script.path} requires ${script.runtime}; the verified sandbox does not advertise a supported runtime.`,
      scriptPath && script.path === scriptPath ? 'error' : 'warning'));
  const compatibility = typeof skill.metadata?.compatibility === 'string' ? skill.metadata.compatibility : null;
  const requirementText = (compatibility ?? '').replace(/\b(?:no|without)\s+(?:extra\s+)?(?:runtime|python|internet|network)(?:\s+access)?\s+(?:is\s+)?required\b/ig, '');
  let compatibilityReviewed = !compatibility;
  let hasUnverifiedCompatibilityRequirement = false;
  if (compatibility) {
    const node = /^(?:Requires?\s+)?Node(?:\.js)?\s*:?\s*((?:>=|<=|>|<|=)?\d+(?:\.\d+){0,2}(?:\s+(?:>=|<=|>|<|=)?\d+(?:\.\d+){0,2})*)?(?:\s*[.;,]\s*(?:no|without)\s+(?:internet|network)(?:\s+access)?(?:\s+(?:is\s+)?required)?)?\s*\.?$/i.exec(compatibility);
    const satisfied = node ? (node[1] ? checkNodeVersion(node[1]) : commands.includes('node') && capabilities.available === true) : null;
    if (satisfied != null) {
      compatibilityReviewed = true;
      if (!satisfied) diagnostics.push(diagnostic('SKILL_RUNTIME_VERSION_UNAVAILABLE', 'The verified Node runtime does not meet the declared version requirement.', 'error'));
    } else {
      // Descriptive compatibility is not a dependency manifest. Explicit unknown requirements still block execution.
      // 描述性兼容说明不是依赖清单；明确声明但无法核验的需求仍阻止执行。
      hasUnverifiedCompatibilityRequirement = /\b(?:requires?|required|needs?|must|depends?\s+on)\b|需要|必须|依赖|要求/u.test(requirementText.toLowerCase()) ||
        /\b(?:python|ruby|java|php|perl|lua|node(?:\.js)?)\s*(?:>=|<=|>|<|=)?\s*\d/i.test(requirementText);
      diagnostics.push(diagnostic('SKILL_COMPATIBILITY_REVIEW', hasUnverifiedCompatibilityRequirement
        ? 'Explicit compatibility requirements need verification before execution.'
        : 'Compatibility text is descriptive; execution is checked against the selected script runtime and dependency manifests.'));
    }
  }
  const declared = skill.metadata?.metadata;
  if (declared && typeof declared === 'object') for (const key of ['dependencies', 'requires']) {
    if (typeof declared[key] === 'string' && declared[key].trim())
      dependencies.push({ ecosystem: 'declared', name: key, requirement: declared[key], source: 'SKILL.md', status: 'unverified' });
  }
  const manifests = inventory.files.filter(file => file.relativePath === 'package.json' ||
    /^scripts\/package\.json$/.test(file.relativePath) || /^requirements(?:[-.][^/]*)?\.txt$/i.test(file.relativePath) ||
    /^scripts\/requirements(?:[-.][^/]*)?\.txt$/i.test(file.relativePath));
  for (const file of manifests) {
    signal?.throwIfAborted();
    const resource = await readSkillResource(inventory.skillRoot, file.relativePath, { signal, denyResource });
    if (resource.kind !== 'text' || resource.hasMore) {
      diagnostics.push(diagnostic('SKILL_DEPENDENCY_MANIFEST_LIMIT', 'Dependency manifest is not bounded readable text; requirements need manual review.', 'error'));
      continue;
    }
    if (basename(file.relativePath) === 'package.json') {
      try {
        const value = JSON.parse(resource.content);
        if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('Invalid package metadata');
        for (const field of ['dependencies', 'optionalDependencies', 'peerDependencies', 'devDependencies']) {
          const required = value[field];
          if (required == null) continue;
          if (!required || typeof required !== 'object' || Array.isArray(required)) throw new Error('Invalid dependency declaration');
          for (const [name, version] of Object.entries(required)) {
            if (typeof version !== 'string') throw new Error('Invalid dependency version');
            dependencies.push({ ecosystem: 'npm', name, requirement: version, source: file.relativePath,
              category: field, status: 'unverified' });
          }
        }
        if (value.engines?.node != null) {
          if (typeof value.engines.node !== 'string') throw new Error('Invalid node engine');
          const satisfied = checkNodeVersion(value.engines.node);
          dependencies.push({ ecosystem: 'runtime', name: 'node', requirement: value.engines.node,
            source: file.relativePath, status: satisfied === true ? 'verified' : satisfied === false ? 'unavailable' : 'unverified' });
        }
      } catch {
        diagnostics.push(diagnostic('SKILL_DEPENDENCY_MANIFEST_INVALID', 'Package dependency metadata is invalid; no installation or lifecycle script was attempted.', 'error'));
      }
    } else {
      for (const line of resource.content.split(/\r?\n/).map(line => line.trim()).filter(line => line && !line.startsWith('#')).slice(0, 128))
        dependencies.push({ ecosystem: 'python', name: line, requirement: line, source: file.relativePath, status: 'unverified' });
    }
  }
  if (dependencies.some(item => item.status !== 'verified'))
    diagnostics.push(diagnostic('SKILL_DEPENDENCIES_UNVERIFIED', 'Declared dependencies have not been verified in the isolated runtime. No package installation or host command was executed.', 'error'));
  if (/\b(?:requires?|required|needs?|must|depends?\s+on)\b|需要|必须|依赖|要求/i.test(requirementText) &&
      /(?:internet|network|https?:\/\/)/i.test(requirementText) &&
      !/(?:no|without)\s+(?:internet|network)/i.test(compatibility) && capabilities.network !== true)
    diagnostics.push(diagnostic('SKILL_NETWORK_UNAVAILABLE', 'The skill mentions network requirements; the current verified sandbox has no network access.', 'error'));
  const hasErrors = diagnostics.some(item => item.severity === 'error');
  const selected = scriptPath ? scripts.find(script => script.path === scriptPath) : scripts.find(script => script.supported);
  return { skillId: skill.id, compatibility, scripts, dependencies,
    sandbox: { available: capabilities.available === true, commands, network: capabilities.network === true,
      skillExecution: skillSandbox, runtimeVersions: { node: process.versions.node } },
    compatible: hasErrors ? false : compatibilityReviewed ? true : null,
    canRun: selected?.supported === true && !hasErrors && !hasUnverifiedCompatibilityRequirement, diagnostics };
}

export function skillPackagePublicInventory(inventory) {
  return { files: inventory.files.map(({ relativePath, size, mimeType }) => ({ path: relativePath, size, mimeType })),
    totalBytes: inventory.totalBytes, truncated: inventory.truncated, diagnostics: inventory.diagnostics };
}
