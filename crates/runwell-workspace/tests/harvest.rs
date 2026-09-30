mod support;
use runwell_workspace::{Error, Excludes, LayerEntry, LayerMetadata, apply_upper};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use support::*;
struct Synthetic(BTreeMap<PathBuf, LayerEntry>);
impl LayerMetadata for Synthetic {
    fn entry(&self, _: &Path, relative: &Path) -> Result<LayerEntry, Error> {
        Ok(self.0.get(relative).cloned().unwrap_or(LayerEntry::Normal))
    }
}
#[test]
fn whiteout_opaque_redirect_and_type_replacement_preserve_lower() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    let target = temp.path().join("target");
    for root in [&lower, &target] {
        put(&root.join("deleted"), b"gone");
        put(&root.join("opaque/stale"), b"stale");
        put(&root.join("moved/original"), b"original");
        put(&root.join("changed"), b"lower");
        put(&root.join("file-to-dir"), b"file");
        put(&root.join("dir-to-file/file"), b"file");
    }
    fs::remove_file(target.join("changed")).unwrap();
    fs::hard_link(lower.join("changed"), target.join("changed")).unwrap();
    put(&upper.join("deleted"), b"synthetic");
    put(&upper.join("opaque/new"), b"new");
    put(&upper.join("moved"), b"synthetic");
    put(&upper.join("renamed/new"), b"new");
    put(&upper.join("changed"), b"upper");
    put(&upper.join("file-to-dir/new"), b"new");
    put(&upper.join("dir-to-file"), b"replacement");
    let markers = Synthetic(BTreeMap::from([
        ("deleted".into(), LayerEntry::Whiteout),
        ("opaque".into(), LayerEntry::Opaque),
        ("moved".into(), LayerEntry::Whiteout),
        ("renamed".into(), LayerEntry::Redirect("moved".into())),
    ]));
    apply_upper(&lower, &upper, &target, &Excludes::new(&[]), &markers).unwrap();
    assert!(!target.join("deleted").exists());
    assert!(!target.join("opaque/stale").exists());
    assert!(!target.join("moved").exists());
    assert_eq!(
        fs::read(target.join("renamed/original")).unwrap(),
        b"original"
    );
    assert_eq!(fs::read(target.join("opaque/new")).unwrap(), b"new");
    assert_eq!(fs::read(target.join("changed")).unwrap(), b"upper");
    assert_eq!(fs::read(lower.join("changed")).unwrap(), b"lower");
    assert!(target.join("file-to-dir/new").exists());
    assert!(target.join("dir-to-file").is_file());
}
#[test]
fn opacity_at_upper_root_discards_the_whole_base() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    let target = temp.path().join("target");
    put(&lower.join("old"), b"old");
    put(&target.join("old"), b"old");
    put(&upper.join("new"), b"new");
    let markers = Synthetic(BTreeMap::from([(PathBuf::new(), LayerEntry::Opaque)]));
    apply_upper(&lower, &upper, &target, &Excludes::new(&[]), &markers).unwrap();
    assert!(!target.join("old").exists());
    assert!(target.join("new").exists());
}
#[test]
fn redirect_cannot_escape_lower_or_promote_an_excluded_source() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    let target = temp.path().join("target");
    put(&lower.join(".ssh/key"), b"secret");
    fs::create_dir_all(upper.join("innocent")).unwrap();
    fs::create_dir(&target).unwrap();
    let markers = Synthetic(BTreeMap::from([(
        "innocent".into(),
        LayerEntry::Redirect(".ssh".into()),
    )]));
    apply_upper(&lower, &upper, &target, &Excludes::new(&[]), &markers).unwrap();
    assert!(!target.join("innocent/key").exists());
    let markers = Synthetic(BTreeMap::from([(
        "innocent".into(),
        LayerEntry::Redirect("../outside".into()),
    )]));
    assert!(apply_upper(&lower, &upper, &target, &Excludes::new(&[]), &markers).is_err());
}

#[test]
fn redirects_preserve_full_path_exclusion_context_at_both_ends() {
    let temp = tempfile::tempdir().unwrap();
    let lower = temp.path().join("lower");
    let upper = temp.path().join("upper");
    let target = temp.path().join("target");
    put(&lower.join(".cargo/config.toml"), b"secret");
    put(&lower.join(".cargo/registry/package"), b"warm");
    fs::create_dir_all(upper.join("renamed")).unwrap();
    fs::create_dir(&target).unwrap();
    let markers = Synthetic(BTreeMap::from([(
        "renamed".into(),
        LayerEntry::Redirect(".cargo".into()),
    )]));
    apply_upper(
        &lower,
        &upper,
        &target,
        &Excludes::new(&["renamed/registry/*".into()]),
        &markers,
    )
    .unwrap();
    assert!(!target.join("renamed/config.toml").exists());
    assert!(!target.join("renamed/registry/package").exists());
}
