use crate::harness::*;

const INVENTORY: &str = "[machine.box-a]\nssh      = \"dibs@box-a\"\nhostname = \"box-a\"\n";

/// A Bash tool call as Claude Code hands it to a PreToolUse hook.
fn bash(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command }
    })
    .to_string()
}

#[test]
fn ssh_to_a_machine_in_the_inventory_is_refused_and_says_why() {
    let mut s = Sandbox::new();
    s.machines(INVENTORY);
    let refused = s
        .dibs(["hook", "ssh"])
        .stdin(&bash("cd /x && ssh box-a nvidia-smi"))
        .run();
    assert_eq!(
        (
            refused.code,
            refused.stdout.as_str(),
            refused
                .stderr
                .lines_with("dibs: ssh reaches box-a around its lock. Nothing was run."),
            refused
                .stderr
                .lines_with("dibs --on box-a --bench <command>  "),
        ),
        (2, "", 1, 1),
        "{}",
        refused.all()
    );
    let passed = s.dibs(["hook", "ssh"]).stdin(&bash("ssh github.com")).run();
    assert_eq!((passed.code, passed.all()), (0, String::new()));
}

#[test]
fn a_call_with_no_shell_command_or_no_inventory_passes() {
    let mut s = Sandbox::new();
    s.machines(INVENTORY);
    for input in [
        r#"{"tool_name":"Read","tool_input":{"file_path":"/x"}}"#,
        "not a tool call",
        "",
    ] {
        let out = s.dibs(["hook", "ssh"]).stdin(input).run();
        assert_eq!((out.code, out.all()), (0, String::new()), "{input}");
    }
    let unknown = s
        .dibs(["hook", "ssh"])
        .env("DIBS_MACHINES", s.p("none.toml"))
        .stdin(&bash("ssh box-a"))
        .run();
    assert_eq!(unknown.code, 0, "no inventory names no machine");
}

#[test]
fn hook_takes_one_kind() {
    let s = Sandbox::new();
    let out = s.dibs(["hook", "scp"]).run();
    assert_eq!(
        (out.code, out.stderr.trim()),
        (2, "dibs: hook takes one kind of hook: ssh")
    );
}

#[test]
fn hook_refuses_a_flag_it_would_drop() {
    let s = Sandbox::new();
    let out = s.dibs(["--on", "box-a", "hook", "ssh"]).run();
    assert_eq!(
        (out.code, out.stderr.trim()),
        (2, "dibs: hook takes no flags, so it would drop --on box-a")
    );
}
