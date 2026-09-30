use runwell_setup::{
    facts::{Fact, HostFacts},
    model::{Applier, Architecture, DryRunApplier, Recommender, RepoTrace, StubRecommender},
    probe, warnings,
};

#[test]
fn complete_linux_fixture_parses_typed_observations() {
    let facts = probe::parse_output(include_bytes!("fixtures/linux.json")).unwrap();
    assert_eq!(facts.vcpus.value, Some(8));
    assert_eq!(
        facts.pressure.value.unwrap().cpu_some.value,
        Some([0.2, 0.4, 0.6])
    );
    assert_eq!(facts.runners.value.unwrap()[0].active_job.value, Some(true));
    assert_eq!(facts.daily_cpu.value.unwrap()[0].p95.value, Some(80.0));
}

#[test]
fn partial_fixture_gives_explicit_unknown_reasons() {
    let facts = probe::parse_output(include_bytes!("fixtures/partial.json")).unwrap();
    assert_eq!(facts.vcpus.value, Some(4));
    assert_eq!(
        facts.ram_bytes.unknown_reason.as_deref(),
        Some("not reported by probe")
    );
    assert_eq!(
        facts.psi.unknown_reason.as_deref(),
        Some("kernel lacks PSI")
    );
    assert!(facts.runners.unknown_reason.is_some());
    let missing: Fact<String> =
        serde_json::from_str("{\"value\":null,\"unknown_reason\":\"\"}").unwrap();
    assert!(missing.unknown_reason.is_some());
}

#[test]
fn garbled_and_truncated_output_is_an_error() {
    for bytes in [
        include_bytes!("fixtures/garbled.json").as_slice(),
        b"{\"os\":",
        b"null",
        b"[]",
        b"{}\n{}",
        b"{\"vcpus\":{\"value\":\"eight\"}}",
    ] {
        assert!(probe::parse_output(bytes).is_err());
    }
}

#[test]
fn no_docker_warning_depends_on_workload() {
    let facts = probe::parse_output(include_bytes!("fixtures/no-docker.json")).unwrap();
    assert_eq!(facts.docker_present.value, Some(false));
    assert!(
        !warnings::derive(&facts, &[])
            .iter()
            .any(|w| w.code == "no_docker")
    );
    let trace = RepoTrace {
        repo: "example/project".parse().unwrap(),
        jobs: vec![],
        uses_service_containers: Some(true),
    };
    let warnings = warnings::derive(&facts, &[trace]);
    for code in ["no_docker", "no_cgroup_v2", "no_psi"] {
        assert!(warnings.iter().any(|w| w.code == code));
    }
}

#[test]
fn warnings_detect_swap_shared_home_old_runners_and_heavy_services() {
    let facts = probe::parse_output(include_bytes!("fixtures/linux.json")).unwrap();
    let warnings = warnings::derive(&facts, &[]);
    for code in [
        "swap_heavily_used",
        "shared_home",
        "old_runner",
        "host_not_dedicated",
    ] {
        assert!(warnings.iter().any(|w| w.code == code));
    }
    assert!(warnings::derive(&HostFacts::default(), &[]).is_empty());
}

#[test]
fn unfinished_recommender_and_dry_run_have_explicit_boundaries() {
    assert!(matches!(
        StubRecommender.recommend(&[], &[]),
        Err(runwell_setup::Error::Unimplemented)
    ));
    let architecture = Architecture {
        name: "example".into(),
        description: "example plan".into(),
    };
    let plan = DryRunApplier.plan(&architecture).unwrap();
    let mut output = Vec::new();
    assert!(DryRunApplier.execute(&plan, "yes", &mut output).is_err());
    assert!(output.is_empty());
    DryRunApplier.execute(&plan, "APPLY", &mut output).unwrap();
    assert!(String::from_utf8(output).unwrap().contains("Dry run"));
}
