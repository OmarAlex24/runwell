use crate::Error;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Mount {
    pub home: PathBuf,
    pub lower: PathBuf,
}
pub(crate) fn scan() -> Result<Vec<Mount>, Error> {
    #[cfg(target_os = "linux")]
    {
        Ok(parse(&std::fs::read("/proc/self/mountinfo")?))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Vec::new())
    }
}
#[cfg(any(target_os = "linux", test))]
fn parse(input: &[u8]) -> Vec<Mount> {
    let mut mounts = Vec::new();
    for line in input.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        match parse_line(line) {
            Ok(entries) => mounts.extend(entries),
            Err(error) => tracing::warn!(%error, "skipping undecodable mountinfo entry"),
        }
    }
    mounts
}
#[cfg(any(target_os = "linux", test))]
fn parse_line(line: &[u8]) -> Result<Vec<Mount>, Error> {
    let line = std::str::from_utf8(line).map_err(|_| Error::Invalid)?;
    let (left, right) = line.split_once(" - ").ok_or(Error::Invalid)?;
    let right: Vec<_> = right.split_whitespace().collect();
    let left: Vec<_> = left.split_whitespace().collect();
    let home = decode(left.get(4).ok_or(Error::Invalid)?)?;
    if right.first() != Some(&"overlay") {
        return Ok(Vec::new());
    }
    let options = right.get(2).ok_or(Error::Invalid)?;
    let mut result = Vec::new();
    for lower in options
        .split(',')
        .filter_map(|o| o.strip_prefix("lowerdir="))
        .flat_map(|l| l.split(':'))
    {
        let lower = PathBuf::from(decode(lower)?);
        let lower =
            if right.get(1) == Some(&"runwell") && lower.file_name().is_some_and(|n| n == "base") {
                std::fs::read_link(lower.join(".generation"))?
            } else {
                lower
            };
        result.push(Mount {
            home: PathBuf::from(&home),
            lower,
        });
    }
    Ok(result)
}
#[cfg(any(target_os = "linux", test))]
fn decode(input: &str) -> Result<String, Error> {
    let mut bytes = Vec::new();
    let mut iter = input.bytes();
    while let Some(c) = iter.next() {
        if c == b'\\' {
            let digits: Vec<_> = iter.by_ref().take(3).collect();
            if digits.len() != 3 || digits.iter().any(|v| !(b'0'..=b'7').contains(v)) {
                return Err(Error::Invalid);
            }
            let n = digits
                .iter()
                .fold(0_u16, |n, d| n * 8 + u16::from(d - b'0'));
            bytes.push(u8::try_from(n).map_err(|_| Error::Invalid)?);
        } else {
            bytes.push(c);
        }
    }
    String::from_utf8(bytes).map_err(|_| Error::Invalid)
}
pub(crate) fn job_id(root: &Path, home: &Path) -> Option<u64> {
    let (relative, private) = if let Ok(relative) = home.strip_prefix(root.join("jobs")) {
        (relative, false)
    } else {
        (home.strip_prefix(root.join("upper")).ok()?, true)
    };
    let mut parts = relative.components();
    let id = parts.next()?.as_os_str().to_str()?.parse().ok()?;
    let tail = parts.as_path();
    if (!private && tail == Path::new("home")) || (private && tail == Path::new("merged")) {
        Some(id)
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_escaped_mount_paths_and_multiple_lowers() {
        let mounts = parse(b"24 1 0:1 / /run/a\\040b/jobs/4/home rw - overlay runwell rw,lowerdir=/cache/a:/cache/b,upperdir=/upper");
        assert_eq!(mounts.len(), 2);
        assert_eq!(job_id(Path::new("/run/a b"), &mounts[0].home), Some(4));
        assert_eq!(mounts[1].lower, Path::new("/cache/b"));
    }
    #[test]
    fn malformed_and_non_utf8_foreign_mounts_do_not_hide_managed_mounts() {
        let mounts = parse(b"invalid\n24 1 0:1 / /foreign\\777 rw - overlay docker rw,lowerdir=/bad\n24 1 0:1 / /foreign\xff rw - overlay docker rw,lowerdir=/bad\n24 1 0:1 / /run/jobs/4/home rw - overlay runwell rw,lowerdir=/cache/good\n");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].lower, Path::new("/cache/good"));
    }
}
