use proptest::prelude::*;
use runwell_admission::{ReservationAdmission, Resources};
use runwell_scheduler::{NodeHeadroom, PendingJob, Priority, Runwell, SchedulingPolicy};
use runwell_sim::{Config, Policy, config::Host};

proptest! {
    #[test]
    fn admission_never_exceeds_scaled_cpu_or_memory(
        cores in 1_u32..128, memory in 1_u64..1_000_000,
        cpu_factor in 0.1_f64..3.0, memory_factor in 0.1_f64..3.0,
        requests in prop::collection::vec((1_u32..64,1_u64..500_000),1..100)) {
        let admission = ReservationAdmission::new(cpu_factor,memory_factor).unwrap();
        let capacity = Resources { cpu_slots:cores,memory_bytes:memory };
        let mut used = Resources::default();
        for (cpu,ram) in requests {
            let request = Resources { cpu_slots:cpu,memory_bytes:ram };
            if admission.fits(capacity,used,request) {
                used.cpu_slots += cpu; used.memory_bytes += ram;
                prop_assert!(f64::from(used.cpu_slots) <= f64::from(cores)*cpu_factor);
                prop_assert!(used.memory_bytes as f64 <= memory as f64*memory_factor);
            }
        }
    }
    #[test]
    fn aging_promotes_old_work_ahead_of_an_adversarial_short_job_stream(
        long in 100_f64..10000.0, threshold in 1_f64..100.0, count in 5_usize..50) {
        let scheduler = Runwell { priority:Priority::Shortest,aging_seconds:threshold,
            admission:ReservationAdmission::new(1.0,1.0).unwrap() };
        let nodes = vec![NodeHeadroom { node_id:0,class:"any".into(),capacity:Resources { cpu_slots:1,memory_bytes:10 },
            reserved:Resources::default(),free_runners:vec![] }];
        let candidate = |id,ready,duration| PendingJob { request_id:id,pool:0,host_class:None,ready_at:ready,
            expected_seconds:duration,critical_path_seconds:duration,fair_service:0.0,reservation:Resources { cpu_slots:1,memory_bytes:1 } };
        let old = candidate(0,0.0,long);
        let mut all = vec![old];
        all.extend((1..count).map(|i| candidate(i,threshold,1.0)));
        let first = scheduler.select(&all,&nodes,threshold).unwrap();
        prop_assert_eq!(first.request_id,0);
        let mut seen = Vec::new();
        let mut now = threshold;
        while let Some(p) = scheduler.select(&all,&nodes,now) {
            seen.push(p.request_id); all.retain(|j| j.request_id != p.request_id); now += long;
        }
        prop_assert_eq!(seen.len(),count);
    }
    #[test]
    fn adding_a_host_does_not_worsen_p90_for_independent_homogeneous_bursts(
        work in prop::collection::vec(1_i64..200,10..80), cores in 1_u32..8) {
        // Independent, simultaneous arrivals with equal resource demand. No claim
        // of universal monotonicity for heterogeneous DAG/list scheduling is made.
        let ts = |x| jiff::Timestamp::from_second(x).unwrap().to_string();
        let jobs: Vec<runwell_trace::TraceJob> = work.iter().enumerate().map(|(i,&duration)| {
            serde_json::from_value(serde_json::json!({"schema_version":1,"repo":"example/app",
                "run_id":i,"event":"pull_request","run_conclusion":"success","run_created_at":ts(0),
                "job_name":format!("job-{i}"),"started_at":ts(i as i64*300),
                "completed_at":ts(i as i64*300+duration),"conclusion":"success","needs":[]})).unwrap()
        }).collect();
        let config = Config { hosts:vec![Host {class:"big".into(),cores,memory_gib:31.0,runners:vec![]},
            Host {class:"small".into(),cores:cores.max(2)/2,memory_gib:15.0,runners:vec![]}], ..Config::default() };
        let result = runwell_sim::simulate(&jobs,&config,&[Policy::Fifo,Policy::Shortest,Policy::CriticalPath,Policy::FairShare]).unwrap();
        for i in 0..4 {
            prop_assert!(result.rows[i+4].metrics.p90_minutes <= result.rows[i].metrics.p90_minutes+1e-9);
            prop_assert_eq!(result.rows[i+4].metrics.runs,work.len());
        }
    }
}

#[test]
fn admission_handles_overflow_and_invalid_factors() {
    let admission = ReservationAdmission::new(1.0, 1.0).unwrap();
    let maximum = Resources {
        cpu_slots: u32::MAX,
        memory_bytes: u64::MAX,
    };
    assert!(!admission.fits(
        maximum,
        maximum,
        Resources {
            cpu_slots: 1,
            memory_bytes: 1
        }
    ));
    assert!(ReservationAdmission::new(f64::NAN, 1.0).is_err());
}

#[test]
fn an_aged_large_job_drains_a_host_instead_of_starving_behind_small_jobs() {
    let scheduler = Runwell {
        priority: Priority::Shortest,
        aging_seconds: 10.0,
        admission: ReservationAdmission::new(1.0, 1.0).unwrap(),
    };
    let large = PendingJob {
        request_id: 0,
        pool: 0,
        host_class: None,
        ready_at: 0.0,
        expected_seconds: 100.0,
        critical_path_seconds: 100.0,
        fair_service: 0.0,
        reservation: Resources {
            cpu_slots: 2,
            memory_bytes: 1,
        },
    };
    let small = PendingJob {
        request_id: 1,
        ready_at: 10.0,
        expected_seconds: 1.0,
        reservation: Resources {
            cpu_slots: 1,
            memory_bytes: 1,
        },
        ..large.clone()
    };
    let mut host = NodeHeadroom {
        node_id: 0,
        class: "big".into(),
        capacity: Resources {
            cpu_slots: 2,
            memory_bytes: 10,
        },
        reserved: Resources {
            cpu_slots: 1,
            memory_bytes: 1,
        },
        free_runners: vec![],
    };
    let pending = [large, small];
    assert!(scheduler.select(&pending, &[host.clone()], 10.0).is_none());
    host.reserved = Resources::default();
    assert_eq!(
        scheduler
            .select(&pending, &[host], 11.0)
            .unwrap()
            .request_id,
        0
    );
}
