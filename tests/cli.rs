use ippin::config::{Config, Store, atomic_json};
use std::{
    net::TcpListener,
    process::{Command, Output},
    time::Duration,
};

fn cli(store: &Store, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ippin"))
        .arg("--home")
        .arg(&store.dir)
        .args(args)
        .env("RUST_LOG", "info")
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cli_errors_are_nonzero_and_invalid_updates_preserve_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().into()).unwrap();
    success(cli(&store, &["add", "--preset", "openai", "*.EXAMPLE.COM"]));
    let cfg = store.load().unwrap();
    assert!(
        cfg.domains.contains(&"example.com".into()) && cfg.domains.contains(&"openai.com".into())
    );
    for args in [
        vec!["add", "bad/domain"],
        vec!["add", "--preset", "unknown"],
        vec!["mode", "http://127.0.0.1:8888"],
        vec!["logs", "-n", "0"],
        vec!["logs", "-n", "-1"],
        vec!["service", "unknown"],
        vec!["start"],
    ] {
        assert!(!cli(&store, &args).status.success(), "{args:?}");
    }
    assert_eq!(store.load().unwrap(), cfg);
    success(cli(&store, &["remove", "example.com"]));
    assert!(
        !store
            .load()
            .unwrap()
            .domains
            .contains(&"example.com".into())
    );
}

#[test]
fn config_updates_from_concurrent_processes_do_not_lose_domains() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().into()).unwrap();
    let children: Vec<_> = (0..8)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_ippin"))
                .arg("--home")
                .arg(&store.dir)
                .args(["add", &format!("test{i}.example.com")])
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let cfg = store.load().unwrap();
    for i in 0..8 {
        assert!(cfg.domains.contains(&format!("test{i}.example.com")));
    }
}

#[test]
fn occupied_listener_fails_without_advertising_a_running_instance() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().into()).unwrap();
    atomic_json(
        &store.path("config.json"),
        &Config {
            port: listener.local_addr().unwrap().port(),
            locked_ip: "203.0.113.1".into(),
            upstream_proxy: "tun".into(),
            ..Config::default()
        },
    )
    .unwrap();
    assert!(!cli(&store, &["start", "--headless"]).status.success());
    assert!(!store.path("instance.json").exists());
    let logs = success(cli(&store, &["logs"]));
    assert!(
        logs.contains("bind local proxy"),
        "startup failure must be recorded: {logs}"
    );
}

#[test]
fn background_process_becomes_ready_stops_gracefully_and_ignores_stale_pids() {
    let available = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = available.local_addr().unwrap().port();
    drop(available);
    let dead_proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = dead_proxy.local_addr().unwrap().port();
    drop(dead_proxy);
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().into()).unwrap();
    // The test never modifies system PAC or contacts the public network.
    atomic_json(
        &store.path("config.json"),
        &Config {
            port,
            auto_win_pac: false,
            locked_ip: "203.0.113.1".into(),
            upstream_proxy: format!("http://127.0.0.1:{proxy_port}"),
            ..Config::default()
        },
    )
    .unwrap();
    struct Cleanup(Store);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = cli(&self.0, &["service", "stop"]);
        }
    }
    let _cleanup = Cleanup(store.clone());
    assert!(success(cli(&store, &["service", "start"])).contains("Process ready"));
    assert!(success(cli(&store, &["service", "status"])).contains("RUNNING"));
    let first = ippin::service::load_instance(&store).unwrap().unwrap();
    success(cli(&store, &["service", "start"]));
    assert_eq!(
        ippin::service::load_instance(&store).unwrap().unwrap().pid,
        first.pid
    );
    assert!(!cli(&store, &["start", "--headless"]).status.success());
    // A forged control token cannot stop a live instance, even with a valid PID.
    let mut forged = first.clone();
    forged.token = "wrong".into();
    forged.save(&store).unwrap();
    assert!(!cli(&store, &["service", "stop"]).status.success());
    first.save(&store).unwrap();
    success(cli(&store, &["service", "stop"]));
    assert!(!store.path("instance.json").exists());
    assert!(success(cli(&store, &["service", "status"])).contains("STOPPED"));
    // A stale PID record referring to this test process must never signal it.
    forged.pid = std::process::id();
    forged.save(&store).unwrap();
    success(cli(&store, &["service", "stop"]));
    assert!(!store.path("instance.json").exists());
    std::thread::sleep(Duration::from_millis(25));
}

#[test]
fn help_without_a_command_succeeds_without_creating_state() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("unused-state");
    let output = Command::new(env!("CARGO_BIN_EXE_ippin"))
        .args(["--color", "never", "--home"])
        .arg(&home)
        .output()
        .unwrap();
    let help = success(output);
    assert!(help.contains("Quick start:") && help.contains("ippin service install"));
    assert!(!help.contains('\u{1b}'));
    assert!(
        !home.exists(),
        "opening help must not initialize or mutate configuration"
    );
}

#[test]
fn pipe_output_and_explicit_color_policies_work_for_commands_help_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().into()).unwrap();
    let plain = success(cli(&store, &["service", "status"]));
    assert!(plain.contains("Process: STOPPED") && !plain.contains('\u{1b}'));
    let colored = success(cli(&store, &["--color", "always", "service", "status"]));
    assert!(colored.contains('\u{1b}') && colored.contains("STOPPED"));
    let help = success(cli(&store, &["--color=always", "--help"]));
    assert!(help.contains('\u{1b}'));
    for (args, env) in [
        (
            vec!["--color", "never", "service", "status"],
            "CLICOLOR_FORCE",
        ),
        (vec!["service", "status"], "NO_COLOR"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_ippin"))
            .arg("--home")
            .arg(&store.dir)
            .args(args)
            .env(env, "1")
            .output()
            .unwrap();
        assert!(!success(output).contains('\u{1b}'));
    }
    let invalid = cli(&store, &["--color", "never", "logs", "-n", "0"]);
    assert!(!invalid.status.success());
    assert!(!String::from_utf8_lossy(&invalid.stderr).contains('\u{1b}'));
}
