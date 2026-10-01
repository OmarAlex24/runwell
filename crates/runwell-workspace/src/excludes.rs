use std::path::Path;

/// Mandatory case-insensitive exclusions, at any depth. Custom rules only add.
/// Symlinks and non-regular files are also never promoted.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".dockercfg",
    ".pgpass",
    ".my.cnf",
    ".terraformrc",
    ".terraform.d/credentials*",
    ".oci",
    ".ansible",
    ".mc",
    ".vault-token",
    ".gem/credentials",
    ".config/gh",
    ".config/gcloud",
    "*.jks",
    "*.keystore",
    "*.p8",
    "*.kdbx",
    "*.gpg",
    "*.asc",
    "*.ovpn",
    ".config",
    ".git",
    ".gitconfig*",
    ".git-credentials",
    ".netrc*",
    "_netrc*",
    ".npmrc*",
    ".yarnrc*",
    ".pypirc*",
    "pip.conf*",
    "bunfig.toml*",
    ".env*",
    "*token*",
    "*credential*",
    "*secret*",
    "*password*",
    "*auth*",
    "*keyring*",
    "*.pem",
    "id_rsa*",
    "id_ed25519*",
    ".boto*",
    ".s3cfg*",
    "*.key",
    "*.p12",
    "*.pfx",
    "*history*",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".zshrc",
    ".zprofile",
    ".cargo/config*",
    ".cargo/credentials*",
    ".m2/settings.xml",
    ".gradle/gradle.properties",
    ".local/share/keyrings",
];
/// Conservative path filter with simple `*`/`?` globs, matched at every depth.
#[derive(Debug, Clone)]
pub struct Excludes(Vec<String>);
impl Excludes {
    /// Extend the mandatory defaults with administrator-supplied relative globs.
    pub fn new(extra: &[String]) -> Self {
        Self(
            DEFAULT_EXCLUDES
                .iter()
                .map(|s| (*s).into())
                .chain(extra.iter().map(|s| s.to_ascii_lowercase()))
                .collect(),
        )
    }
    /// Non-UTF8 names are excluded rather than bypassing credential rules.
    pub fn contains(&self, path: &Path) -> bool {
        let Some(path) = path.to_str() else {
            return true;
        };
        if path.is_empty() {
            return false;
        }
        let path = path.to_ascii_lowercase();
        let parts: Vec<_> = path.split('/').collect();
        (0..parts.len()).any(|start| {
            (start + 1..=parts.len()).any(|end| {
                let candidate = parts[start..end].join("/");
                self.0
                    .iter()
                    .any(|pattern| matches(pattern.as_bytes(), candidate.as_bytes()))
            })
        })
    }
}
fn matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut p, mut v, mut star, mut backtrack) = (0, 0, None, 0);
    while v < value.len() {
        if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == value[v]) {
            p += 1;
            v += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            backtrack = v;
        } else if let Some(s) = star {
            backtrack += 1;
            v = backtrack;
            p = s + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}
