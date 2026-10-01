use runwell_transport::{Identity, Tls, certs};

#[test]
fn issuance_and_rotation_never_overwrite_keys_and_keep_private_modes() {
    let temp = tempfile::tempdir().unwrap();
    let ca = temp.path().join("ca");
    certs::create_ca(&ca, 30).unwrap();
    let old_key = std::fs::read(ca.join("ca-key.pem")).unwrap();
    assert!(certs::create_ca(&ca, 30).is_err());
    assert_eq!(old_key, std::fs::read(ca.join("ca-key.pem")).unwrap());
    for generation in ["old", "new"] {
        let dir = temp.path().join(generation);
        certs::issue(&ca, &dir, &Identity::node("node-1"), 7).unwrap();
        assert!(
            Tls::load(&runwell_config::TransportConfig {
                ca_file: dir.join("ca.pem"),
                certificate_file: dir.join("identity.pem"),
                private_key_file: dir.join("key.pem")
            })
            .is_ok()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.join("key.pem"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
    assert_ne!(
        std::fs::read(temp.path().join("old/key.pem")).unwrap(),
        std::fs::read(temp.path().join("new/key.pem")).unwrap()
    );
    assert!(
        certs::issue(
            &ca,
            &temp.path().join("bad"),
            &Identity::node("../escape"),
            7
        )
        .is_err()
    );
}
