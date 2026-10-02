use super::{job, time};
use crate::{fetch, trace_build};
use serde_json::json;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

#[test]
fn skipped_dependency_does_not_discard_the_successful_run_graph() {
    let mut jobs = vec![
        job("build", 1, 0, 0, 10),
        job("optional", 1, 10, 10, 10),
        job("finish", 1, 10, 11, 12),
    ];
    jobs[1].conclusion = Some("skipped".into());
    assert!(
        trace_build::apply_workflow(
            &mut jobs,
            "jobs:\n  build: {}\n  optional:\n    needs: build\n  finish:\n    needs: optional\n"
        )
        .unwrap()
    );
    let report = crate::analyze(&jobs, &super::args(&[]), super::rules(), Vec::new()).unwrap();
    assert_eq!(report.runs[0].needs_runs, 1);
    assert_eq!(report.runs[0].end_to_end_seconds.p50, Some(12.0));
}

#[test]
fn dynamic_matrix_maps_observed_children_and_preserves_dispatch_gap() {
    let yaml = r#"
jobs:
  plan: {}
  build:
    name: Package (${{ matrix.package }})
    needs: plan
    strategy:
      matrix: ${{ fromJSON(needs.plan.outputs.packages) }}
      max-parallel: 2
  finish:
    needs: build
"#;
    let mut jobs = vec![
        job("plan", 1, 2, 3, 8),
        job("Package (one)", 1, 10, 12, 20),
        job("Package (two)", 1, 15, 20, 30),
        job("finish", 1, 32, 34, 40),
    ];
    assert!(trace_build::apply_workflow(&mut jobs, yaml).unwrap());
    assert_eq!(jobs[1].needs, Some(vec!["plan".into()]));
    assert_eq!(jobs[1].max_parallel, Some(2));
    assert_eq!(jobs[2].dispatch_delay_seconds, Some(2.0));
    assert_eq!(
        jobs[3].needs,
        Some(vec!["Package (one)".into(), "Package (two)".into()])
    );
}

#[test]
fn ambiguous_matrix_names_do_not_partially_mutate_trace() {
    let yaml = r#"
jobs:
  first:
    name: Test (${{ matrix.kind }})
    strategy: {matrix: {kind: [a]}}
  second:
    name: Test (${{ matrix.kind }})
    strategy: {matrix: {kind: [a]}}
"#;
    let mut jobs = vec![job("Test (a)", 1, 0, 1, 10)];
    let original = jobs.clone();
    assert!(!trace_build::apply_workflow(&mut jobs, yaml).unwrap());
    assert_eq!(jobs, original);
}

#[tokio::test]
async fn dynamic_workflow_is_not_requested_from_repository_contents() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    Mock::given(path("/repos/acme/app/actions/runs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total_count": 1,
            "workflow_runs": [{"id": 1, "created_at": time(0), "event": "dynamic",
                "path": "dynamic/provider/generated", "head_sha": "synthetic-sha"}]
        })))
        .mount(&server)
        .await;
    Mock::given(path("/repos/acme/app/actions/runs/1/attempts/1/jobs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"jobs": []})))
        .mount(&server)
        .await;
    let mut client =
        fetch::Client::new(server.uri(), "synthetic-token".into(), dir.path().into()).unwrap();
    let result = fetch::collect(&mut client, &["acme/app".into()], time(0), time(100), 0)
        .await
        .unwrap();
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("Generated workflow"))
    );
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.url.path().contains("/contents/"))
    );
}
