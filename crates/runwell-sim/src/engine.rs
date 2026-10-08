use crate::{Config, Error, Policy, PreparedTrace};
use runwell_admission::Resources;
use runwell_scheduler::{
    Baseline, NodeHeadroom, PendingJob, Runwell, SchedulingPolicy, SemaphoreDecision, heavy_slot,
};
use std::{cmp::Ordering, collections::BinaryHeap};

mod cancellation;
mod capacity;
mod placement;
mod production;
mod progress;

const EPS: f64 = 1e-7;
#[derive(Clone, Default)]
enum Phase {
    Before,
    Protected,
    #[default]
    Whole,
}
#[derive(Debug, Clone, Copy, PartialEq)]
struct Event {
    time: f64,
    job: usize,
}
impl Eq for Event {}
impl Ord for Event {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .time
            .total_cmp(&self.time)
            .then(other.job.cmp(&self.job))
    }
}
impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Default)]
pub(crate) struct Timing {
    pub ready: f64,
    pub start: f64,
    pub end: f64,
    pub failure: bool,
    pub host: Option<usize>,
    pub semaphore_wait: f64,
    phase: Phase,
    runner: Option<usize>,
    done: bool,
    cancelled: bool,
    held_at: f64,
    poll_at: f64,
    slot: bool,
    remaining: f64,
    failure_exposure: f64,
    elapsed: f64,
}
pub(crate) struct Outcome {
    pub timings: Vec<Timing>,
    pub cpu_utilization: f64,
    pub memory_utilization: f64,
    pub fail_opens: usize,
    pub cancelled_runs: Vec<bool>,
}
struct Engine<'a> {
    trace: &'a PreparedTrace,
    config: &'a Config,
    policy: Policy,
    selector: Box<dyn SchedulingPolicy>,
    fair_state: runwell_scheduler::FairState,
    duration_windows:
        std::collections::BTreeMap<runwell_scheduler::HistoryKey, std::collections::VecDeque<f64>>,
    criticality: Vec<runwell_scheduler::Criticality>,
    now: f64,
    nodes: Vec<NodeHeadroom>,
    runner_slots: Vec<Vec<runwell_scheduler::RunnerSlots>>,
    use_runners: bool,
    host_offline: Vec<usize>,
    capacity_changes: Vec<crate::availability::Change>,
    capacity_agenda: BinaryHeap<Event>,
    agenda: BinaryHeap<Event>,
    cancel_agenda: BinaryHeap<Event>,
    cancelled_runs: Vec<bool>,
    children: Vec<Vec<usize>>,
    pending: Vec<usize>,
    parent_end: Vec<f64>,
    ready: Vec<usize>,
    held: Vec<usize>,
    running: Vec<usize>,
    slots: Vec<usize>,
    service: Vec<f64>,
    matrix_active: Vec<usize>,
    timings: Vec<Timing>,
    finished: usize,
    cpu_area: f64,
    memory_area: f64,
    fail_opens: usize,
}

pub(crate) fn replay(
    trace: &PreparedTrace,
    config: &Config,
    policy: Policy,
    hosts: usize,
) -> Result<Outcome, Error> {
    replay_allocation(trace, config, policy, hosts, None)
}
pub(crate) fn replay_allocation(
    trace: &PreparedTrace,
    config: &Config,
    policy: Policy,
    hosts: usize,
    allocation: Option<&[Vec<usize>]>,
) -> Result<Outcome, Error> {
    if allocation.is_some_and(|a| {
        a.len() != hosts || a.iter().any(|row| row.len() != trace.pool_limits.len())
    }) {
        return Err(Error::Invalid(
            "runner allocation must be host by pool".into(),
        ));
    }
    let selector: Box<dyn SchedulingPolicy> = match policy.priority() {
        None => Box::new(Baseline),
        Some(priority) => {
            let policy = Runwell {
                priority,
                aging_seconds: config.aging_seconds,
                admission: config.admission()?,
            };
            if config.runwell_runner_availability {
                Box::new(runwell_scheduler::RunwellWithRunners(policy))
            } else {
                Box::new(policy)
            }
        }
    };
    let nodes: Vec<_> = config.hosts[..hosts]
        .iter()
        .enumerate()
        .map(|(i, h)| NodeHeadroom {
            node_id: i,
            class: h.class.clone(),
            capacity: h.resources(),
            reserved: Resources::default(),
            free_runners: allocation.map_or_else(|| trace.pool_limits.clone(), |a| a[i].clone()),
        })
        .collect();
    let use_runners = policy.runner_limited() || config.runwell_runner_availability;
    let changes = capacity::timeline(trace, use_runners, allocation, hosts);
    let mut engine = Engine {
        trace,
        config,
        policy,
        selector,
        fair_state: runwell_scheduler::FairState::default(),
        duration_windows: std::collections::BTreeMap::new(),
        criticality: if policy == Policy::Production {
            runwell_scheduler::graph_criticality(
                &trace
                    .jobs
                    .iter()
                    .map(|j| j.needs.clone())
                    .collect::<Vec<_>>(),
            )?
        } else {
            Vec::new()
        },
        now: 0.0,
        runner_slots: nodes
            .iter()
            .map(|n| {
                n.free_runners
                    .iter()
                    .map(|&limit| runwell_scheduler::RunnerSlots::new(limit))
                    .collect()
            })
            .collect(),
        use_runners,
        host_offline: vec![0; hosts],
        capacity_agenda: changes
            .iter()
            .enumerate()
            .map(|(i, c)| Event {
                time: c.time,
                job: i,
            })
            .collect(),
        capacity_changes: changes,
        nodes,
        agenda: BinaryHeap::new(),
        cancel_agenda: trace
            .runs
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.cancel_at.map(|time| Event { time, job: i }))
            .collect(),
        cancelled_runs: vec![false; trace.runs.len()],
        children: vec![Vec::new(); trace.jobs.len()],
        pending: trace.jobs.iter().map(|j| j.needs.len()).collect(),
        parent_end: trace
            .jobs
            .iter()
            .map(|j| trace.runs[j.run].arrival)
            .collect(),
        ready: Vec::new(),
        held: Vec::new(),
        running: Vec::new(),
        slots: vec![0; hosts],
        service: vec![0.0; trace.repos.len()],
        matrix_active: vec![0; trace.jobs.len()],
        timings: vec![Timing::default(); trace.jobs.len()],
        finished: 0,
        cpu_area: 0.0,
        memory_area: 0.0,
        fail_opens: 0,
    };
    engine.validate_placement()?;
    for (i, j) in trace.jobs.iter().enumerate() {
        for &p in &j.needs {
            engine.children[p].push(i);
        }
        if j.needs.is_empty() {
            engine.agenda.push(Event {
                time: trace.runs[j.run].arrival + j.delay,
                job: i,
            });
        }
    }
    let beginning = trace
        .runs
        .iter()
        .map(|r| r.arrival)
        .fold(f64::INFINITY, f64::min);
    while engine.finished < trace.jobs.len() {
        engine.complete();
        engine.cancel_due();
        engine.capacity_due();
        loop {
            while engine
                .agenda
                .peek()
                .is_some_and(|e| e.time <= engine.now + EPS)
            {
                if let Some(e) = engine.agenda.pop() {
                    if engine.timings[e.job].done {
                        continue;
                    }
                    engine.timings[e.job].ready = e.time;
                    let job = &trace.jobs[e.job];
                    if job.work == 0.0 || !job.local {
                        engine.timings[e.job].start = engine.now + job.external_queue;
                        engine.timings[e.job].remaining = job.work + job.external_queue;
                        engine.running.push(e.job);
                    } else {
                        engine.ready.push(e.job);
                    }
                }
            }
            if engine
                .running
                .iter()
                .all(|&i| engine.timings[i].remaining > EPS)
            {
                break;
            }
            engine.complete();
        }
        engine.start_held()?;
        engine.place()?;
        if engine.finished == trace.jobs.len() {
            break;
        }
        let speeds = engine.speeds();
        let mut next = engine.agenda.peek().map_or(f64::INFINITY, |e| e.time);
        next = next.min(
            engine
                .capacity_agenda
                .peek()
                .map_or(f64::INFINITY, |e| e.time),
        );
        next = next.min(
            engine
                .cancel_agenda
                .peek()
                .map_or(f64::INFINITY, |e| e.time),
        );
        for (&i, &rate) in engine.running.iter().zip(&speeds) {
            next = next.min(engine.now + engine.timings[i].remaining.max(0.0) / rate);
        }
        for &i in &engine.held {
            if engine.timings[i]
                .host
                .is_some_and(|h| engine.host_offline[h] > 0)
            {
                continue;
            }
            next = next.min(engine.timings[i].poll_at);
        }
        if !next.is_finite() {
            return Err(Error::Invalid(
                "no progress: unschedulable job or blocked dependency".into(),
            ));
        }
        engine.advance((next - engine.now).max(0.0), &speeds);
        engine.now = next.max(engine.now);
    }
    let duration = (engine.now - beginning).max(EPS);
    let cores = engine
        .nodes
        .iter()
        .map(|n| f64::from(n.capacity.cpu_slots))
        .sum::<f64>();
    let ram = engine
        .nodes
        .iter()
        .map(|n| n.capacity.memory_bytes as f64)
        .sum::<f64>();
    Ok(Outcome {
        timings: engine.timings,
        cpu_utilization: engine.cpu_area / (duration * cores),
        memory_utilization: engine.memory_area / (duration * ram),
        fail_opens: engine.fail_opens,
        cancelled_runs: engine.cancelled_runs,
    })
}
