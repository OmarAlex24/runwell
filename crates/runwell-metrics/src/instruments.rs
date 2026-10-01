use crate::Error;
use prometheus_client::{
    encoding::{EncodeLabelSet, EncodeLabelValue},
    metrics::{counter::Counter, family::Family, gauge::Gauge, histogram::Histogram},
    registry::Registry,
};
use std::{collections::BTreeSet, sync::atomic::AtomicU64};
type FloatGauge = Gauge<f64, AtomicU64>;
type Histograms = Family<Class, Histogram, fn() -> Histogram>;
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Class {
    class: String,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Node {
    node: String,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Admission {
    class: String,
    decision: AdmissionDecision,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Pressure {
    node: String,
    resource: PressureResource,
}
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ResultLabel {
    class: String,
    conclusion: CompletionKind,
}
/// Finite admission decision labels.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum AdmissionDecision {
    /// Host accepted a reservation.
    Accepted,
    /// Reservation resources exhausted.
    Capacity,
    /// PSI brake engaged.
    Pressure,
    /// Node not usable (stale, offline, draining).
    Unavailable,
}
/// Finite resource pressure labels; values are percentages from PSI avg10.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum PressureResource {
    /// CPU some pressure.
    Cpu,
    /// Memory some pressure.
    Memory,
    /// I/O some pressure.
    Io,
}
/// Completed workflow outcome used for infra failure rate denominators.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, EncodeLabelValue)]
pub enum CompletionKind {
    /// Successful execution.
    Success,
    /// Confirmed infrastructure failure.
    Infra,
    /// Test or code failure.
    TestCode,
    /// Unclassified outcome.
    Unknown,
}
/// Registry with class/node allowlists. Unknown labels collapse to `other`,
/// preventing job IDs, repositories and arbitrary peer input from growing series.
#[derive(Debug)]
pub struct Metrics {
    registry: Registry,
    classes: BTreeSet<String>,
    nodes: BTreeSet<String>,
    queue: Histograms,
    runtime: Histograms,
    admission: Family<Admission, Counter>,
    psi: Family<Pressure, FloatGauge>,
    retries: Family<Class, Counter>,
    completed: Family<ResultLabel, Counter>,
    infra_rate: FloatGauge,
    cpu: Family<Node, Gauge>,
    memory: Family<Node, Gauge>,
}
fn seconds() -> Histogram {
    Histogram::new([
        0.1, 1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0,
    ])
}
impl Default for Metrics {
    fn default() -> Self {
        Self::build(BTreeSet::new(), BTreeSet::new())
    }
}
impl Metrics {
    /// Register all metrics with at most 64 configured classes and 256 nodes.
    pub fn new(classes: BTreeSet<String>, nodes: BTreeSet<String>) -> Result<Self, Error> {
        if classes.len() > 64
            || nodes.len() > 256
            || classes
                .iter()
                .chain(&nodes)
                .any(|v| v.is_empty() || v.len() > 128)
        {
            return Err(Error::Invalid);
        }
        Ok(Self::build(classes, nodes))
    }
    fn build(classes: BTreeSet<String>, nodes: BTreeSet<String>) -> Self {
        let mut metrics = Self {
            registry: Registry::default(),
            classes,
            nodes,
            queue: Family::new_with_constructor(seconds as fn() -> Histogram),
            runtime: Family::new_with_constructor(seconds as fn() -> Histogram),
            admission: Family::default(),
            psi: Family::default(),
            retries: Family::default(),
            completed: Family::default(),
            infra_rate: FloatGauge::default(),
            cpu: Family::default(),
            memory: Family::default(),
        };
        let r = &mut metrics.registry;
        r.register(
            "runwell_queue_seconds",
            "Ready to execution queue time",
            metrics.queue.clone(),
        );
        r.register(
            "runwell_run_seconds",
            "Execution duration excluding queue time",
            metrics.runtime.clone(),
        );
        r.register(
            "runwell_admission_decisions",
            "Authoritative admission decisions",
            metrics.admission.clone(),
        );
        r.register(
            "runwell_psi_percent",
            "Host PSI some avg10 percentage",
            metrics.psi.clone(),
        );
        r.register(
            "runwell_retries",
            "Accepted automatic infra job retries by reservation class",
            metrics.retries.clone(),
        );
        r.register(
            "runwell_completed_jobs",
            "Completed jobs by failure classification",
            metrics.completed.clone(),
        );
        r.register(
            "runwell_infra_failure_ratio",
            "Infra failures divided by all completions in the alert window",
            metrics.infra_rate.clone(),
        );
        r.register(
            "runwell_node_headroom_cpu_slots",
            "Admission reported remaining CPU slots",
            metrics.cpu.clone(),
        );
        r.register(
            "runwell_node_headroom_memory_bytes",
            "Admission reported remaining memory bytes",
            metrics.memory.clone(),
        );
        metrics
    }
    /// Registry extension boundary; callers remain responsible for bounded labels.
    pub fn registry(&mut self) -> &mut Registry {
        &mut self.registry
    }
    /// Encode OpenMetrics text suitable for a Prometheus scrape endpoint.
    pub fn encode(&self) -> Result<String, Error> {
        let mut output = String::new();
        prometheus_client::encoding::text::encode(&mut output, &self.registry)
            .map_err(|_| Error::Encoding)?;
        Ok(output)
    }
    /// Record queue and runtime once per authoritative completion.
    pub fn completed(
        &self,
        class: &str,
        queue_seconds: f64,
        run_seconds: f64,
        conclusion: CompletionKind,
    ) -> Result<(), Error> {
        if [queue_seconds, run_seconds]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err(Error::Invalid);
        }
        let class = label(&self.classes, class);
        self.queue
            .get_or_create(&Class {
                class: class.clone(),
            })
            .observe(queue_seconds);
        self.runtime
            .get_or_create(&Class {
                class: class.clone(),
            })
            .observe(run_seconds);
        self.completed
            .get_or_create(&ResultLabel { class, conclusion })
            .inc();
        Ok(())
    }
    /// Increment one authoritative reservation outcome.
    pub fn admission(&self, class: &str, decision: AdmissionDecision) {
        self.admission
            .get_or_create(&Admission {
                class: label(&self.classes, class),
                decision,
            })
            .inc();
    }
    /// Record one accepted automatic job retry (not HTTP send attempts).
    pub fn retry(&self, class: &str) {
        self.retries
            .get_or_create(&Class {
                class: label(&self.classes, class),
            })
            .inc();
    }
    /// Update a bounded PSI sample.
    pub fn pressure(
        &self,
        node: &str,
        resource: PressureResource,
        percent: f64,
    ) -> Result<(), Error> {
        if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
            return Err(Error::Invalid);
        }
        self.psi
            .get_or_create(&Pressure {
                node: label(&self.nodes, node),
                resource,
            })
            .set(percent);
        Ok(())
    }
    /// Update admission's available resources, after reservations and OS headroom.
    pub fn headroom(&self, node: &str, cpu_slots: u32, memory_bytes: u64) -> Result<(), Error> {
        let memory_bytes = i64::try_from(memory_bytes).map_err(|_| Error::Invalid)?;
        let node = Node {
            node: label(&self.nodes, node),
        };
        self.cpu.get_or_create(&node).set(i64::from(cpu_slots));
        self.memory.get_or_create(&node).set(memory_bytes);
        Ok(())
    }
    /// Update from the same window used by the in-process alert evaluator.
    pub fn infra_failure_ratio(&self, ratio: f64) -> Result<(), Error> {
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return Err(Error::Invalid);
        }
        self.infra_rate.set(ratio);
        Ok(())
    }
}
fn label(allowed: &BTreeSet<String>, value: &str) -> String {
    if allowed.contains(value) {
        value.into()
    } else {
        "other".into()
    }
}
