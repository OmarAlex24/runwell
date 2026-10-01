//! Read actual runner renewal evidence, never filesystem mtime or process liveness.
//! GitHub Runner's JobDispatcher emits this trace only after a successful renewal.
use crate::Error;
use rustix::fs::{Dir, Mode, OFlags, open, openat};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) fn latest(directory: &Path) -> Result<Option<i64>, Error> {
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::DIRECTORY;
    let parent = open(
        directory.parent().ok_or(Error::Config)?,
        flags,
        Mode::empty(),
    );
    let parent = match parent {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(Error::Io),
    };
    let directory = match openat(&parent, "_diag", flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(_) => return Err(Error::Io),
    };
    // Rotation is lexical UTC. Keep only the two newest names and bounded tails;
    // old evidence is retained in the durable lease, not rediscovered via history.
    let mut names = std::collections::BTreeSet::new();
    for entry in Dir::read_from(&directory)
        .map_err(|_| Error::Io)?
        .take(4096)
    {
        let entry = entry.map_err(|_| Error::Io)?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if name.starts_with("Runner_") && name.ends_with(".log") {
            names.insert(name.to_owned());
            if names.len() > 2 {
                names.pop_first();
            }
        }
    }
    let mut newest = None;
    for name in names {
        let fd = match openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(_) => continue,
        };
        let mut file = File::from(fd);
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let offset = metadata.len().saturating_sub(64 * 1024);
        file.seek(SeekFrom::Start(offset))?;
        let mut tail = Vec::new();
        file.take(64 * 1024).read_to_end(&mut tail)?;
        let text = String::from_utf8_lossy(&tail);
        for line in text.lines().skip(usize::from(offset > 0)) {
            if let Some(at) = renewal(line) {
                newest = Some(newest.map_or(at, |old: i64| old.max(at)));
            }
        }
    }
    Ok(newest)
}
fn renewal(line: &str) -> Option<i64> {
    let (time, message) = line
        .strip_prefix('[')?
        .split_once(" INFO JobDispatcher] ")?;
    if !message.starts_with("Successfully renew job ") || !message.contains(", job is valid till ")
    {
        return None;
    }
    time.parse::<jiff::Timestamp>()
        .ok()
        .map(|t| t.as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_successful_runner_renewals_refresh_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let diag = dir.path().join("_diag");
        std::fs::create_dir(&diag).unwrap();
        let log = diag.join("Runner_20261001-120000-utc.log");
        std::fs::write(&log, "[2026-10-01 12:00:00Z INFO JobDispatcher] Successfully renew job request 123, job is valid till later\n").unwrap();
        let expected = "2026-10-01T12:00:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_millisecond();
        assert_eq!(latest(&diag).unwrap(), Some(expected));
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(
            file,
            "[2026-10-01 12:05:00Z INFO JobDispatcher] Attempting renewal"
        )
        .unwrap();
        writeln!(
            file,
            "[2026-10-01 12:05:01Z INFO Other] Successfully renew job 123, job is valid till later"
        )
        .unwrap();
        assert_eq!(latest(&diag).unwrap(), Some(expected));
        writeln!(file, "[2026-10-01 12:06:00Z INFO JobDispatcher] Successfully renew job 123, job is valid till later").unwrap();
        assert_eq!(latest(&diag).unwrap(), Some(expected + 360_000));
    }
    #[test]
    fn rotated_logs_symlinks_and_incomplete_lines_do_not_invent_heartbeats() {
        let dir = tempfile::tempdir().unwrap();
        let diag = dir.path().join("_diag");
        std::fs::create_dir(&diag).unwrap();
        let log = diag.join("Runner_20261001-120000-utc.log");
        std::fs::write(&log, "partial renewal\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&log, diag.join("Runner_20261001-120100-utc.log")).unwrap();
        assert_eq!(latest(&diag).unwrap(), None);
    }
}
