use crate::{Error, Excludes, disk, harvest::multiple_links};
use std::{fs, io::Read, path::Path};

pub(crate) fn apply(
    base: &Path,
    home: &Path,
    target: &Path,
    excludes: &Excludes,
) -> Result<(), Error> {
    walk(base, home, target, Path::new(""), excludes)
}
fn walk(
    base: &Path,
    home: &Path,
    target: &Path,
    relative: &Path,
    excludes: &Excludes,
) -> Result<(), Error> {
    if excludes.contains(relative) {
        return disk::remove(target);
    }
    let m = match fs::symlink_metadata(home) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return disk::remove(target),
        Err(e) => return Err(e.into()),
    };
    let old = fs::symlink_metadata(base).ok();
    if m.is_dir() {
        if old.as_ref().is_some_and(|m| m.is_dir())
            && !fs::symlink_metadata(target).is_ok_and(|m| m.is_dir())
            && unchanged_directory(base, home, relative, excludes)?
        {
            return Ok(());
        }
        if fs::symlink_metadata(target).is_ok_and(|m| !m.is_dir()) {
            disk::remove(target)?;
        }
        disk::directory(target, 0o700)?;
        if old.is_some_and(|m| m.is_dir()) {
            for e in fs::read_dir(base)? {
                let e = e?;
                if fs::symlink_metadata(home.join(e.file_name())).is_err() {
                    disk::remove(&target.join(e.file_name()))?;
                }
            }
        }
        for e in fs::read_dir(home)? {
            let e = e?;
            walk(
                &base.join(e.file_name()),
                &e.path(),
                &target.join(e.file_name()),
                &relative.join(e.file_name()),
                excludes,
            )?;
        }
    } else if m.is_file() && !multiple_links(&m) {
        if !old.is_some_and(|old| old.is_file() && old.len() == m.len()) || !same(base, home)? {
            disk::copy_file(home, target)?;
        }
    } else {
        disk::remove(target)?;
    }
    Ok(())
}
fn same(a: &Path, b: &Path) -> Result<bool, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(a)?.permissions().mode() & 0o111
            != fs::metadata(b)?.permissions().mode() & 0o111
        {
            return Ok(false);
        }
    }
    if fs::metadata(a)?.modified()? != fs::metadata(b)?.modified()? {
        return Ok(false);
    }
    let mut a = fs::File::open(a)?;
    let mut b = fs::File::open(b)?;
    let mut x = [0; 65536];
    let mut y = [0; 65536];
    loop {
        let n = a.read(&mut x)?;
        let m = b.read(&mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

fn unchanged_directory(
    base: &Path,
    home: &Path,
    relative: &Path,
    excludes: &Excludes,
) -> Result<bool, Error> {
    let mut names = std::collections::BTreeSet::new();
    for root in [base, home] {
        for e in fs::read_dir(root)? {
            names.insert(e?.file_name());
        }
    }
    for name in names {
        let rel = relative.join(&name);
        if excludes.contains(&rel) {
            continue;
        }
        let a = base.join(&name);
        let b = home.join(name);
        let (Ok(old), Ok(new)) = (fs::symlink_metadata(&a), fs::symlink_metadata(&b)) else {
            return Ok(false);
        };
        if old.is_dir() && new.is_dir() {
            if !unchanged_directory(&a, &b, &rel, excludes)? {
                return Ok(false);
            }
        } else if !old.is_file()
            || !new.is_file()
            || multiple_links(&new)
            || old.len() != new.len()
            || !same(&a, &b)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}
