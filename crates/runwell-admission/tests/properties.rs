use proptest::prelude::*;
use runwell_admission::*;
proptest! {
    #[test]
    fn reservations_never_exceed_scaled_capacity(cpu in 1_u32..128, memory in 1_u64..1_000_000,
        cpu_factor in 0.1_f64..4.0, mem_factor in 0.1_f64..4.0, max in 1_u32..100,
        requests in prop::collection::vec((1_u32..64,1_u64..100_000,any::<bool>()),1..300)) {
        let capacity = Resources { cpu_slots: cpu, memory_bytes: memory };
        let policy = ReservationAdmission::new(cpu_factor,mem_factor).unwrap();
        let limit = policy.conservative_limit(capacity);
        let mut host = HostAdmission::new(capacity,policy,max);
        let mut count = 0;
        for (id,(cpu,memory,paused)) in requests.into_iter().enumerate() {
            if host.reserve(id as u64,Resources { cpu_slots: cpu,memory_bytes: memory },if paused { Brake::Paused } else { Brake::Open }) { count += 1; }
            prop_assert!(host.used().cpu_slots <= limit.cpu_slots);
            prop_assert!(host.used().memory_bytes <= limit.memory_bytes);
            prop_assert!(count <= max);
        }
    }
    #[test]
    fn hysteresis_transitions_never_flap_faster_than_dwell(dwell in 1_u64..1000,
        samples in prop::collection::vec((0_u64..500,0.0_f64..100.0,any::<bool>()),1..1000)) {
        let mut brake = PsiBrake::new([Threshold { high: 20.0,low: 10.0 };3],dwell).unwrap();
        let mut time = 0;
        let mut changed = 0;
        let mut previous = brake.state();
        for (dt,value,full) in samples {
            time += dt;
            let decision = brake.update(time,Pressure { cpu: value,memory: value,io: value,memory_full: if full { 1.0 } else { 0.0 } });
            if full { prop_assert_eq!(decision,Brake::Paused); }
            if brake.state() != previous {
                prop_assert!(time - changed >= dwell);
                changed = time; previous = brake.state();
            }
        }
    }
}
#[test]
fn recovery_requires_sustained_low_pressure_and_restore_cannot_grant_capacity() {
    let mut brake = PsiBrake::new(
        [Threshold {
            high: 20.0,
            low: 10.0,
        }; 3],
        100,
    )
    .unwrap();
    assert_eq!(
        brake.update(
            100,
            Pressure {
                cpu: 21.0,
                ..Default::default()
            }
        ),
        Brake::Paused
    );
    assert_eq!(brake.update(200, Pressure::default()), Brake::Paused);
    assert_eq!(
        brake.update(
            250,
            Pressure {
                io: 15.0,
                ..Default::default()
            }
        ),
        Brake::Paused
    );
    assert_eq!(brake.update(300, Pressure::default()), Brake::Paused);
    assert_eq!(brake.update(400, Pressure::default()), Brake::Open);
    let r = Resources {
        cpu_slots: 1,
        memory_bytes: 1,
    };
    let mut host = HostAdmission::new(r, ReservationAdmission::new(1.0, 1.0).unwrap(), 1);
    host.restore(
        1,
        Resources {
            cpu_slots: 2,
            memory_bytes: 2,
        },
    );
    assert!(!host.can_admit(r, Brake::Open));
    host.release(1);
    assert!(host.reserve(2, r, Brake::Open));
    assert!(host.reserve(2, r, Brake::Open));
    assert!(!host.reserve(3, r, Brake::Open));
}

#[test]
fn large_integer_capacity_is_not_rounded_up_and_initial_high_pressure_vetoes() {
    let capacity = Resources {
        cpu_slots: u32::MAX,
        memory_bytes: u64::MAX - 1,
    };
    assert_eq!(
        ReservationAdmission::new(1.0, 1.0)
            .unwrap()
            .conservative_limit(capacity),
        capacity
    );
    let mut brake = PsiBrake::new(
        [Threshold {
            high: 20.0,
            low: 10.0,
        }; 3],
        30_000,
    )
    .unwrap();
    assert_eq!(
        brake.update(
            0,
            Pressure {
                cpu: 50.0,
                ..Default::default()
            }
        ),
        Brake::Paused
    );
    assert_eq!(brake.update(1, Pressure::default()), Brake::Paused);
}
