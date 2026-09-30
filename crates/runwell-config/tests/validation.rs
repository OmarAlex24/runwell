use runwell_config::{AuthConfig, Config};

const EXAMPLE: &str = include_str!("../../../examples/runwell.toml");

#[test]
fn example_configuration_is_valid() {
    assert!(Config::from_toml(EXAMPLE).is_ok());
}

#[test]
fn unknown_keys_are_rejected() {
    let source = EXAMPLE.replace(
        "schema_version = 1",
        "schema_version = 1\nunrecognized = true",
    );
    assert!(Config::from_toml(&source).is_err());
}

#[test]
fn invalid_resource_budgets_are_rejected() {
    for source in [
        EXAMPLE.replace("cpu_slots = 8", "cpu_slots = 0"),
        EXAMPLE.replace("cpu_slots = 4", "cpu_slots = 9"),
        EXAMPLE.replace("memory_high_bytes = 2147483648", "memory_high_bytes = 0"),
        EXAMPLE.replace(
            "memory_high_bytes = 2147483648",
            "memory_high_bytes = 4294967296",
        ),
        EXAMPLE.replace(
            "memory_max_bytes = 10737418240",
            "memory_max_bytes = 21474836480",
        ),
        EXAMPLE.replace("cpu_weight = 100", "cpu_weight = 10001"),
    ] {
        assert!(Config::from_toml(&source).is_err());
    }
}

#[test]
fn duplicate_class_names_are_rejected() {
    assert!(Config::from_toml(&EXAMPLE.replace("runwell-large", "runwell-small")).is_err());
}

#[test]
fn invalid_hysteresis_is_rejected() {
    for threshold in ["10.0", "11.0", "-1.0", "nan", "inf"] {
        let source = EXAMPLE.replace(
            "resume_percent = 5.0",
            &format!("resume_percent = {threshold}"),
        );
        assert!(Config::from_toml(&source).is_err());
    }
}

#[test]
fn relative_secret_paths_are_rejected() {
    let source = EXAMPLE.replace("/run/credentials/runwell/github-app.pem", "github-app.pem");
    assert!(Config::from_toml(&source).is_err());
}

#[test]
fn pat_configuration_is_supported() {
    let source = EXAMPLE.replace(
        r#"kind = "app"
app_id = 1
installation_id = 1
private_key_file = "/run/credentials/runwell/github-app.pem""#,
        r#"kind = "pat"
token_file = "/run/credentials/runwell/github-token""#,
    );
    let config = Config::from_toml(&source).expect("PAT example should validate");
    assert!(matches!(config.github.auth, AuthConfig::Pat { .. }));
}
