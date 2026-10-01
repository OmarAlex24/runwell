mod support;
use proptest::prelude::*;
use runwell_scheduler::*;
use support::*;

proptest! {
    #[test]
    fn deterministic_under_snapshot_permutation(requests in prop::collection::vec((1_u32..12,1_u64..100),1..40)) {
        let mut snapshot = ProductionSnapshot::default();
        let jobs:Vec<_> = requests.iter().enumerate().map(|(i,&(cpu,memory))| {
            let mut j=job(i,&format!("repo{}",i%3),&format!("pr{}",i%5),&mut snapshot);
            j.reservation.cpu_slots=cpu;j.reservation.memory_bytes=memory;j
        }).collect();
        let nodes=vec![node(1,16,100,&mut snapshot),node(0,32,200,&mut snapshot)];
        let config=ProductionConfig::default();let state=FairState::default();
        let policy=Production::new(&config,&snapshot,&state).unwrap();
        let first=policy.decide(&jobs,&nodes,1.0);
        let reversed:Vec<_>=jobs.into_iter().rev().collect();
        let reversed_nodes:Vec<_>=nodes.into_iter().rev().collect();
        prop_assert_eq!(first,policy.decide(&reversed,&reversed_nodes,1.0));
    }
    #[test]
    fn a_sequence_of_admissions_never_exceeds_reported_headroom(cpu in 1_u32..32,memory in 1_u64..1000,requests in prop::collection::vec((1_u32..16,1_u64..300),1..80),limit in 1_u32..20) {
        let mut snapshot=ProductionSnapshot::default();
        let mut jobs:Vec<_>=requests.iter().enumerate().map(|(i,&(c,m))| {let mut j=job(i,"a","1",&mut snapshot);j.reservation.cpu_slots=c;j.reservation.memory_bytes=m;j}).collect();
        let mut nodes=vec![node(0,cpu,memory,&mut snapshot)];
        snapshot.nodes.get_mut(&0).unwrap().remaining_jobs=limit;
        let mut state=FairState::default();let config=ProductionConfig::default();let mut admitted=0;
        while let Some(d)=Production::new(&config,&snapshot,&state).unwrap().decide(&jobs,&nodes,1.0) {
            let j=jobs.iter().find(|j|j.request_id==d.placement.request_id).unwrap();
            nodes[0].reserved.cpu_slots+=j.reservation.cpu_slots;nodes[0].reserved.memory_bytes+=j.reservation.memory_bytes;
            prop_assert!(nodes[0].reserved.cpu_slots<=cpu);prop_assert!(nodes[0].reserved.memory_bytes<=memory);
            snapshot.nodes.get_mut(&0).unwrap().remaining_jobs-=1;admitted+=1;prop_assert!(admitted<=limit);
            jobs.retain(|j|j.request_id!=d.placement.request_id);state=d.fairness;
        }
    }
    #[test]
    fn aged_backlog_is_fifo_despite_endless_new_critical_short_jobs(count in 1_usize..50,threshold in 1_f64..1000.0) {
        let mut snapshot=ProductionSnapshot::default();
        let mut jobs:Vec<_>=(0..count).map(|i|job(i,"old","pr",&mut snapshot)).collect();
        let nodes=vec![node(0,1,1,&mut snapshot)];
        let config=ProductionConfig {aging_seconds:threshold,..Default::default()};let mut state=FairState::default();
        for expected in 0..count {
            let id=count+expected;let mut young=job(id,"burst","pr",&mut snapshot);young.ready_at=threshold;young.expected_seconds=0.01;jobs.push(young);
            snapshot.jobs.get_mut(&id).unwrap().criticality=Some(Criticality {depth:100,fan_out:1000});
            let d=Production::new(&config,&snapshot,&state).unwrap().decide(&jobs,&nodes,threshold).unwrap();
            prop_assert_eq!(d.placement.request_id,expected);jobs.retain(|j|j.request_id!=expected);state=d.fairness;
        }
    }
    #[test]
    fn large_aged_work_protects_capacity_until_existing_reservations_finish(cores in 2_u32..64,threshold in 1_f64..100.0) {
        let mut snapshot=ProductionSnapshot::default();let mut large=job(0,"old","pr",&mut snapshot);large.reservation.cpu_slots=cores;
        let mut small=job(1,"new","pr",&mut snapshot);small.ready_at=threshold;
        let mut nodes=vec![node(0,cores,10,&mut snapshot)];nodes[0].reserved.cpu_slots=1;
        let config=ProductionConfig {aging_seconds:threshold,..Default::default()};let state=FairState::default();
        let policy=Production::new(&config,&snapshot,&state).unwrap();
        prop_assert!(policy.select(&[large.clone(),small.clone()],&nodes,threshold).is_none());
        nodes[0].reserved.cpu_slots=0;
        prop_assert_eq!(policy.select(&[large,small],&nodes,threshold+1.0).unwrap().request_id,0);
    }
    #[test]
    fn repository_and_pr_dispatch_shares_converge_to_weights(a in 1_u32..9,b in 1_u32..9,x in 1_u32..6,y in 1_u32..6) {
        let mut snapshot=ProductionSnapshot::default();let jobs=vec![job(0,"a","x",&mut snapshot),job(1,"a","y",&mut snapshot),job(2,"b","z",&mut snapshot)];
        let nodes=vec![node(0,1,1,&mut snapshot)];
        let config=ProductionConfig {repository_weights:[("a".into(),a),("b".into(),b)].into(),pr_weights:[(("a".into(),"x".into()),x),(("a".into(),"y".into()),y)].into(),..Default::default()};
        let mut state=FairState::default();let mut counts=[0_u32;3];
        let rounds=(a+b)*(x+y)*4;
        for _ in 0..rounds {
            let d=Production::new(&config,&snapshot,&state).unwrap().decide(&jobs,&nodes,0.0).unwrap();counts[d.placement.request_id]+=1;state=d.fairness;
        }
        prop_assert_eq!(counts[0]+counts[1],a*(x+y)*4);prop_assert_eq!(counts[2],b*(x+y)*4);
        prop_assert_eq!(counts[0],a*x*4);prop_assert_eq!(counts[1],a*y*4);
    }
}
