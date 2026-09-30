use runwell_node::{cgroup::*, service_unit, slice_unit};
#[test]
fn cgroup_parsers_record_cpu_io_pressure_and_oom_without_silent_zeroes() {
    assert_eq!(
        counter(
            "usage_usec 123\nuser_usec 100\nsystem_usec 23\n",
            "usage_usec"
        )
        .unwrap(),
        123
    );
    assert_eq!(
        counter("low 0\nhigh 4\nmax 8\noom 2\noom_kill 1\n", "oom_kill").unwrap(),
        1
    );
    assert_eq!(
        io_bytes("8:0 rbytes=4 wbytes=8 rios=1 wios=2\n8:1 rbytes=6 wbytes=9").unwrap(),
        (10, 17)
    );
    let psi = "some avg10=2.50 avg60=1.50 avg300=0.1 total=200\nfull avg10=1.0 avg60=0.5 avg300=0.0 total=100\n";
    assert_eq!(
        pressure_line(psi, "some").unwrap(),
        PressureLine {
            avg10: 2.5,
            total: 200
        }
    );
    assert_eq!(pressure_line(psi, "full").unwrap().total, 100);
    for bad in [
        "",
        "some avg10=nan total=0",
        "some avg10=101 total=0",
        "some avg10=1",
    ] {
        assert!(pressure_line(bad, "some").is_err());
    }
    assert!(counter("usage_usec bad", "usage_usec").is_err());
    assert!(counter("", "oom_kill").is_err());
}
#[test]
fn every_job_slice_has_ci_as_systemd_ancestor() {
    for id in [1, 2, 100, u64::MAX] {
        let name = slice_unit(id);
        let parts: Vec<_> = name.trim_end_matches(".slice").split('-').collect();
        assert_eq!(parts[0], "ci");
        assert_eq!(name, format!("ci-rw-j{id}.slice"));
        assert_eq!(service_unit(id), format!("rw-j{id}.service"));
    }
}
