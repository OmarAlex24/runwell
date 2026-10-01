mod support;
use proptest::prelude::*;
use runwell_workspace::{DEFAULT_EXCLUDES, Excludes};
use std::{fs, path::Path};
use support::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]
    #[test]
    fn secrets_never_promoted(prefix in "[a-z]{0,12}", suffix in "[a-z]{0,12}", content in prop::collection::vec(any::<u8>(), 0..256), upper_case in any::<bool>()) {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = cache(temp.path());
        let home = cache.prepare(1, Some(key())).unwrap();
        let name = format!("{prefix}token{suffix}");
        let name = if upper_case { name.to_ascii_uppercase() } else { name };
        put(&home.join("nested").join(&name).join("innocent"), &content);
        put(&home.join(".ssh/id_rsa"), &content);
        put(&home.join(".docker/config.json"), &content);
        put(&home.join(".git-credentials"), &content);
        put(&home.join("go/pkg/cache/safe"), b"warm");
        cache.promote(1, &success(), 1).unwrap();
        let next = cache.prepare(2, Some(key())).unwrap();
        prop_assert!(!next.join("nested").join(name).exists());
        prop_assert!(!next.join(".ssh").exists());
        prop_assert!(!next.join(".docker").exists());
        prop_assert!(!next.join(".git-credentials").exists());
        prop_assert_eq!(fs::read(next.join("go/pkg/cache/safe")).unwrap(), b"warm");
    }
}
#[test]
fn defaults_are_mandatory_and_custom_excludes_apply_at_any_depth() {
    let excludes = Excludes::new(&["company/private/*".into()]);
    assert!(DEFAULT_EXCLUDES.contains(&"*token*"));
    for path in [
        "a/.ssh/id_rsa",
        "x/.docker/config.json",
        ".cargo/credentials.toml",
        "deep/credential-helper",
        "deep/.git/config",
        "a/.npmrc",
        "a/.env.production",
        "company/private/data",
    ] {
        assert!(excludes.contains(Path::new(path)), "{path}");
    }
    for path in [
        "go/pkg/mod/cache/download/a",
        ".cache/go-build/a",
        ".bun/install/cache/a",
        ".cache/ms-playwright/chromium/chrome",
        ".npm/_cacache/content/a",
    ] {
        assert!(!excludes.contains(Path::new(path)), "{path}");
    }
}
#[cfg(unix)]
#[test]
fn symlinks_and_hardlink_aliases_do_not_launder_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    let home = cache.prepare(1, Some(key())).unwrap();
    put(&home.join(".ssh/id"), b"secret");
    fs::hard_link(home.join(".ssh/id"), home.join("apparently-safe")).unwrap();
    std::os::unix::fs::symlink(".ssh/id", home.join("alias")).unwrap();
    std::os::unix::fs::symlink("/etc/passwd", home.join("external")).unwrap();
    cache.promote(1, &success(), 1).unwrap();
    let next = cache.prepare(2, Some(key())).unwrap();
    assert!(!next.join("apparently-safe").exists());
    assert!(!next.join("alias").exists());
    assert!(!next.join("external").exists());
}

#[test]
fn expanded_credentials_are_excluded_case_insensitively_at_every_depth() {
    let temp = tempfile::tempdir().unwrap();
    let mut cache = cache(temp.path());
    let home = cache.prepare(1, Some(key())).unwrap();
    let names = [
        ".dockercfg",
        ".pgpass",
        ".my.cnf",
        ".netrc",
        ".terraformrc",
        ".terraform.d/credentials.tfrc.json",
        "client.jks",
        "client.keystore",
        "client.p8",
        "client.p12",
        "client.pfx",
        "client.kdbx",
        "client.gpg",
        "client.asc",
        "client.ovpn",
        ".oci/config",
        ".ansible/cache",
        ".mc/config.json",
        ".vault-token",
        ".npmrc",
        ".pypirc",
        ".gem/credentials",
        ".config/gh/hosts.yml",
        ".config/gcloud/config",
        ".azure/access",
        ".kube/config",
    ];
    let excludes = Excludes::new(&[]);
    for name in names {
        for name in [name.to_owned(), name.to_ascii_uppercase()] {
            let path = Path::new("nested").join(name);
            assert!(excludes.contains(&path), "{}", path.display());
            put(&home.join(path), b"never share");
        }
    }
    put(&home.join(".cache/go-build/safe"), b"warm");
    cache.promote(1, &success(), 1).unwrap();
    let next = cache.prepare(2, Some(key())).unwrap();
    for name in names {
        for name in [name.to_owned(), name.to_ascii_uppercase()] {
            assert!(!next.join("nested").join(name).exists());
        }
    }
    assert_eq!(
        fs::read(next.join(".cache/go-build/safe")).unwrap(),
        b"warm"
    );
}
