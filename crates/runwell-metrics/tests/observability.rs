use runwell_metrics::*;
#[test]
fn metrics_encode_bounded_labels_and_reject_invalid_measurements() {
    let m = Metrics::new(["small".into()].into(), ["node1".into()].into()).unwrap();
    m.completed("small", 2.0, 5.0, CompletionKind::Infra)
        .unwrap();
    m.retry("small");
    m.admission("small", AdmissionDecision::Accepted);
    m.pressure("node1", PressureResource::Memory, 4.0).unwrap();
    m.headroom("node1", 2, 4096).unwrap();
    m.infra_failure_ratio(0.02).unwrap();
    for i in 0..1000 {
        m.retry(&format!("untrusted-{i}"));
    }
    let output = m.encode().unwrap();
    for name in [
        "runwell_queue_seconds_bucket",
        "runwell_run_seconds_sum",
        "runwell_admission_decisions_total",
        "runwell_psi_percent",
        "runwell_retries_total",
        "runwell_completed_jobs_total",
        "runwell_infra_failure_ratio",
        "runwell_node_headroom_cpu_slots",
        "runwell_node_headroom_memory_bytes",
    ] {
        assert!(output.contains(name), "{name}");
    }
    assert!(!output.contains("untrusted-"));
    assert!(output.contains("class=\"other\"} 1000"));
    assert!(
        m.completed("small", f64::NAN, 1.0, CompletionKind::Unknown)
            .is_err()
    );
    assert!(m.pressure("node1", PressureResource::Cpu, 101.0).is_err());
}
#[test]
fn rolling_rate_is_strictly_over_one_percent_and_deduplicates_expired_future_events() {
    let mut snapshot = AlertSnapshot::default();
    let config = AlertConfig {
        window_seconds: 100,
        ..Default::default()
    };
    snapshot.completions = (0..100)
        .map(|i| Completion {
            id: i.to_string(),
            at: 100,
            infra: i == 0,
        })
        .collect();
    snapshot.completions.push(snapshot.completions[0].clone());
    snapshot.completions.push(Completion {
        id: "old".into(),
        at: 0,
        infra: true,
    });
    snapshot.completions.push(Completion {
        id: "future".into(),
        at: 101,
        infra: true,
    });
    assert_eq!(snapshot.infra_ratio(100, 100), (100, 0.01));
    assert!(evaluate(&config, &snapshot, 100).unwrap().is_empty());
    snapshot.completions[1].infra = true;
    assert_eq!(
        evaluate(&config, &snapshot, 100).unwrap()[0].kind,
        AlertKind::InfraFailureRate
    );
    assert!(evaluate(&config, &snapshot, 201).unwrap().is_empty());
}
#[test]
fn stuck_job_missing_node_and_template_expiry_alert_at_their_boundaries() {
    let snapshot = AlertSnapshot {
        jobs: vec![JobWatch {
            id: "job/1/running".into(),
            since: 0,
            p90_seconds: 10.0,
        }],
        nodes: vec![NodeWatch {
            id: "node1".into(),
            last_report: 0,
        }],
        templates: vec![TemplateWatch {
            version: "v1".into(),
            expires_at: 60,
        }],
        ..Default::default()
    };
    let config = AlertConfig {
        heartbeat_seconds: 30,
        template_lead_seconds: 10,
        ..Default::default()
    };
    assert!(evaluate(&config, &snapshot, 30).unwrap().is_empty());
    let alerts = evaluate(&config, &snapshot, 50).unwrap();
    assert_eq!(alerts.len(), 3);
    assert!(alerts.iter().any(|a| a.kind == AlertKind::StuckJob));
    assert!(alerts.iter().any(|a| a.kind == AlertKind::NodeMissing));
    assert!(alerts.iter().any(|a| a.kind == AlertKind::TemplateExpiry));
}
