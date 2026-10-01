use crate::harness::*;

#[test]
fn what_a_test_starts_inherits_no_descriptor_but_its_streams() {
    // Under the cargo wrapper this process holds its lock on a descriptor, and anything a test
    // left running with that descriptor would stop every cargo on the computer.
    let s = Sandbox::new();
    let open = s
        .sh("for n in $(seq 0 63); do [ -e /dev/fd/$n ] && echo $n; done; true")
        .run()
        .stdout;
    let fds: Vec<&str> = open.split_whitespace().collect();
    assert_eq!(
        fds,
        ["0", "1", "2"],
        "a test's process sees only its three streams"
    );
}
