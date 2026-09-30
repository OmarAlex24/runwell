//! Strict, pure cgroup-v2 parsers; Linux readers use rustix file descriptors.
use crate::Error;

/// One PSI some/full row.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PressureLine {
    /// Ten-second exponentially weighted average, in percent.
    pub avg10: f64,
    /// Cumulative stalled microseconds.
    pub total: u64,
}
/// Parse one PSI row, rejecting missing or invalid counters rather than admitting
/// work on a fabricated zero-pressure sample.
pub fn pressure_line(text: &str, kind: &str) -> Result<PressureLine, Error> {
    let line = text
        .lines()
        .find(|line| line.split_whitespace().next() == Some(kind))
        .ok_or(Error::Stats)?;
    let mut avg = None;
    let mut total = None;
    for token in line.split_whitespace().skip(1) {
        if let Some(v) = token.strip_prefix("avg10=") {
            let value: f64 = v.parse().map_err(|_| Error::Stats)?;
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(Error::Stats);
            }
            avg = Some(value);
        }
        if let Some(v) = token.strip_prefix("total=") {
            total = Some(v.parse().map_err(|_| Error::Stats)?);
        }
    }
    Ok(PressureLine {
        avg10: avg.ok_or(Error::Stats)?,
        total: total.ok_or(Error::Stats)?,
    })
}
/// Read a required numeric counter from cpu.stat or memory.events.
pub fn counter(text: &str, name: &str) -> Result<u64, Error> {
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some(name) {
            return fields
                .next()
                .ok_or(Error::Stats)?
                .parse()
                .map_err(|_| Error::Stats);
        }
    }
    Err(Error::Stats)
}
/// Sum device read/write byte counters; an empty io.stat means no block I/O.
pub fn io_bytes(text: &str) -> Result<(u64, u64), Error> {
    let (mut read, mut write) = (0_u64, 0_u64);
    for line in text.lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some(v) = field.strip_prefix("rbytes=") {
                read = read
                    .checked_add(v.parse().map_err(|_| Error::Stats)?)
                    .ok_or(Error::Stats)?;
            }
            if let Some(v) = field.strip_prefix("wbytes=") {
                write = write
                    .checked_add(v.parse().map_err(|_| Error::Stats)?)
                    .ok_or(Error::Stats)?;
            }
        }
    }
    Ok((read, write))
}

#[cfg(target_os = "linux")]
mod read {
    use super::*;
    use runwell_admission::Pressure;
    use runwell_store::{JobMeasurement, PsiMeasurement};
    use rustix::fs::{Mode, OFlags, open};
    use std::{
        io::Read,
        path::{Path, PathBuf},
    };
    pub fn text(path: &Path) -> Result<String, Error> {
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| Error::Io)?;
        let mut text = String::new();
        std::fs::File::from(fd)
            .take(1024 * 1024)
            .read_to_string(&mut text)?;
        Ok(text)
    }
    pub fn pressure(root: &Path, host: bool) -> Result<PsiMeasurement, Error> {
        let path = |name: &str| {
            root.join(if host {
                name.to_owned()
            } else {
                format!("{name}.pressure")
            })
        };
        let cpu = pressure_line(&text(&path("cpu"))?, "some")?;
        let memory_text = text(&path("memory"))?;
        let memory = pressure_line(&memory_text, "some")?;
        let full = pressure_line(&memory_text, "full")?;
        let io = pressure_line(&text(&path("io"))?, "some")?;
        Ok(PsiMeasurement {
            cpu: cpu.avg10,
            memory: memory.avg10,
            io: io.avg10,
            memory_full: full.avg10,
            cpu_total: cpu.total,
            memory_total: memory.total,
            io_total: io.total,
            memory_full_total: full.total,
        })
    }
    pub fn host_pressure() -> Result<Pressure, Error> {
        let host = pressure(Path::new("/proc/pressure"), true)?;
        let ci = pressure(Path::new("/sys/fs/cgroup/ci.slice"), false)?;
        Ok(Pressure {
            cpu: host.cpu.max(ci.cpu),
            memory: host.memory.max(ci.memory),
            io: host.io.max(ci.io),
            memory_full: host.memory_full.max(ci.memory_full),
        })
    }
    pub fn path(id: u64) -> PathBuf {
        PathBuf::from("/sys/fs/cgroup/ci.slice/ci-rw.slice").join(crate::slice_unit(id))
    }
    pub fn measure(id: u64) -> Result<JobMeasurement, Error> {
        let root = path(id);
        let mut result = JobMeasurement {
            job_id: id as i64,
            ..Default::default()
        };
        if !root.try_exists()? {
            return Ok(result);
        }
        result.cpu_usec = counter(&text(&root.join("cpu.stat"))?, "usage_usec")?;
        let number = |name| {
            text(&root.join(name))?
                .trim()
                .parse()
                .map_err(|_| Error::Stats)
        };
        result.memory_current = number("memory.current")?;
        result.memory_peak = number("memory.peak")?;
        result.oom_kills = counter(&text(&root.join("memory.events"))?, "oom_kill")?;
        (result.io_read_bytes, result.io_write_bytes) = io_bytes(&text(&root.join("io.stat"))?)?;
        result.psi = pressure(&root, false)?;
        result.infra_signal = result.oom_kills > 0;
        Ok(result)
    }
}
#[cfg(target_os = "linux")]
pub(crate) use read::{host_pressure, measure};
