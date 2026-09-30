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
        parse(&std::fs::read_to_string("/proc/self/mountinfo")?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Vec::new())
    }
}
#[cfg(any(target_os = "linux", test))]
fn parse(input: &str) -> Result<Vec<Mount>, Error> {
    let mut mounts = Vec::new();
    for line in input.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            return Err(Error::Invalid);
        };
        let right: Vec<_> = right.split_whitespace().collect();
        if right.first() != Some(&"overlay") {
            continue;
        }
        let left: Vec<_> = left.split_whitespace().collect();
        let home = decode(left.get(4).ok_or(Error::Invalid)?)?;
        let options = right.get(2).ok_or(Error::Invalid)?;
        for lower in options
            .split(',')
            .filter_map(|o| o.strip_prefix("lowerdir="))
            .flat_map(|l| l.split(':'))
        {
            mounts.push(Mount {
                home: PathBuf::from(&home),
                lower: decode(lower)?.into(),
            });
        }
    }
    Ok(mounts)
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
    let relative = home.strip_prefix(root.join("jobs")).ok()?;
    let mut parts = relative.components();
    let id = parts.next()?.as_os_str().to_str()?.parse().ok()?;
    if parts.next()?.as_os_str() != "home" || parts.next().is_some() {
        return None;
    }
    Some(id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_escaped_mount_paths_and_multiple_lowers() {
        let mounts = parse("24 1 0:1 / /run/a\\040b/jobs/4/home rw - overlay runwell rw,lowerdir=/cache/a:/cache/b,upperdir=/upper").unwrap();
        assert_eq!(mounts.len(), 2);
        assert_eq!(job_id(Path::new("/run/a b"), &mounts[0].home), Some(4));
        assert_eq!(mounts[1].lower, Path::new("/cache/b"));
    }
}
