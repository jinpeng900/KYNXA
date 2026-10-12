/// Classify sensitive names before opening a source; content-based classification would already expose credentials.
/// 在打开来源前判断敏感名称；按内容分类会提前暴露凭据。
pub(super) fn is_sensitive(path: &str) -> bool {
    let lower = path.to_lowercase();
    let components = lower
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let filename = components.last().copied().unwrap_or("");
    filename == ".env"
        || filename.starts_with(".env.")
        || [
            ".npmrc",
            ".pypirc",
            ".netrc",
            ".git-credentials",
            "credentials",
            "credentials.json",
            "secrets.json",
            "secrets.yaml",
            "secrets.yml",
            "application_default_credentials.json",
            "accesstokens.json",
        ]
        .contains(&filename)
        || components
            .iter()
            .any(|part| [".ssh", ".gnupg"].contains(part))
        || super::intent::matches(r"^(?:id_(?:rsa|dsa|ecdsa|ed25519))(?:\.|$)", filename)
        || super::intent::matches(r"\.(?:p12|pfx|key)$", filename)
}
