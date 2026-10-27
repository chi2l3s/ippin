use anyhow::{Context, Result, ensure};
use clap::{Args, ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
mod ui;
use ippin::{
    checker::{Checker, IpProbe},
    config::{Store, preset},
    gate::Gate,
    platform,
    proxy::Server,
    service::{self, Instance},
    transport::{self, Transport},
};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::Arc,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, prelude::*};
use ui::Tone;

#[derive(Parser)]
#[command(
    name = "ippin",
    bin_name = "ippin",
    version,
    about = "Keep selected services behind one pinned IP.",
    styles = ui::help_styles(),
    help_template = "{name} {version}\n{about-with-newline}\n{usage-heading} {usage}\n\n{all-args}{after-help}",
    after_help = "Quick start:\n  ippin mode http://127.0.0.1:2080\n  ippin lock\n  ippin start -w\n\nWindows autostart:  ippin service install\nRecent events:     ippin logs -n 50\n\nUse -w only on Windows. Full guide: https://github.com/chi2l3s/ippin"
)]
struct Cli {
    /// State directory (default: IPPIN_HOME or ~/.ippin).
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    /// Control terminal colors (also honors NO_COLOR in auto mode).
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: ColorChoice,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Detect and pin the external IP through the configured route.
    Lock {
        #[arg(short, long)]
        upstream: Option<String>,
        #[arg(long)]
        ipv6: bool,
    },
    /// Show configuration, process readiness, route and current IP.
    Status,
    /// Set auto, tun, http://127.0.0.1:PORT or socks5://127.0.0.1:PORT.
    Mode { mode: String },
    /// Add protected domains or a built-in preset.
    Add {
        domains: Vec<String>,
        #[arg(long)]
        preset: Option<String>,
    },
    /// Remove protected domains from the PAC rules.
    Remove {
        #[arg(required = true)]
        domains: Vec<String>,
    },
    /// Run the gatekeeper until Ctrl+C or 'service stop'.
    Start(StartArgs),
    /// Manage the background process and Windows login autostart.
    Service {
        #[arg(value_enum)]
        action: ServiceAction,
    },
    /// Read recent log records (up to 10,000).
    Logs {
        #[arg(short = 'n', long = "lines", default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=10000))]
        lines: u32,
    },
    /// Enable or restore Windows system PAC settings.
    Win {
        #[arg(value_enum)]
        action: WinAction,
    },
}

#[derive(Args)]
struct StartArgs {
    /// Override listening port for this process only.
    #[arg(short, long, value_parser = clap::value_parser!(u16).range(1..))]
    port: Option<u16>,
    /// Set Windows system PAC for the lifetime of this process.
    #[arg(short = 'w', long = "win-pac")]
    win_pac: bool,
    #[arg(long, hide = true)]
    headless: bool,
}
#[derive(Clone, Copy, ValueEnum)]
enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
    Restart,
    Status,
}
#[derive(Clone, Copy, ValueEnum)]
enum WinAction {
    Enable,
    Disable,
}

#[tokio::main(worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let color = ui::color_choice(&args);
    ui::init(color);
    let matches = Cli::command().color(color).get_matches_from(args);
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    if cli.command.is_none() {
        let result = Cli::command().color(color).print_help();
        anstream::println!();
        return if result.is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    match run(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "command failed");
            ui::error(format!("{e:#}"));
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let store = Store::new(match cli.home {
        Some(path) => path,
        None => Store::default_path()?,
    })?;
    match cli.command.context("command missing")? {
        Command::Start(args) => start(&store, args).await,
        Command::Lock { upstream, ipv6 } => {
            ui::title("Pin external IP");
            let mut cfg = store.load()?;
            let original = cfg.clone();
            if let Some(mode) = upstream {
                cfg.upstream_proxy = mode;
            }
            cfg.validate()?;
            let route = transport::resolve(&cfg.upstream_proxy, cfg.port).await?;
            ui::field("Route", route.description());
            if ui::interactive() {
                ui::hint("Checking external IP through this route…");
            }
            let observation = Checker::new(Transport::new(route, ipv6, cfg.port), ipv6)
                .fetch()
                .await?;
            store.update(|current| {
                ensure!(
                    *current == original,
                    "configuration changed during IP detection; retry 'ippin lock'"
                );
                current.upstream_proxy = cfg.upstream_proxy.clone();
                current.locked_ip = observation.ip.to_string();
                current.locked_country = observation.country.clone();
                Ok(())
            })?;
            ui::field(
                "Pinned IP",
                format!("{} ({})", observation.ip, observation.country),
            );
            ui::success("IP pinned. New requests must match this address.");
            changed(&store).await?;
            Ok(())
        }
        Command::Mode { mode } => {
            ui::title("Outbound route");
            let cfg = store.update(|cfg| {
                cfg.upstream_proxy = mode;
                Ok(())
            })?;
            ui::field("Mode", cfg.upstream_proxy);
            if ui::interactive() {
                ui::success("Outbound mode saved.");
            }
            changed(&store).await
        }
        Command::Add {
            domains,
            preset: name,
        } => {
            ui::title("Protected domains");
            ensure!(
                !domains.is_empty() || name.is_some(),
                "provide domains or --preset claude|openai|gemini"
            );
            let cfg = store.update(|cfg| {
                if let Some(name) = name {
                    for domain in preset(&name)? {
                        cfg.add_domain(domain)?;
                    }
                }
                for domain in domains {
                    cfg.add_domain(&domain)?;
                }
                Ok(())
            })?;
            ui::success("Protected domains updated.");
            ui::field("Domains", cfg.domains.join(", "));
            changed(&store).await
        }
        Command::Remove { domains } => {
            ui::title("Protected domains");
            let domains: Vec<_> = domains
                .iter()
                .map(|s| ippin::config::normalize_domain(s))
                .collect::<Result<_>>()?;
            let cfg = store.update(|cfg| {
                cfg.domains.retain(|domain| !domains.contains(domain));
                Ok(())
            })?;
            ui::success("Protected domains updated.");
            ui::field("Domains", cfg.domains.join(", "));
            changed(&store).await
        }
        Command::Status => {
            ui::title("Status");
            let cfg = store.load()?;
            let running = service::running(&store).await?;
            ui::section("Process");
            if let Some(instance) = &running {
                ui::state(
                    "Process",
                    "RUNNING",
                    format!("PID {}, port {}", instance.pid, instance.port),
                    Tone::Success,
                );
                ui::field("Active route", &instance.route);
            } else {
                ui::state("Process", "STOPPED", "", Tone::Warning);
            }
            ui::section("Network");
            ui::field("Configured mode", &cfg.upstream_proxy);
            ui::field(
                "Pinned IP",
                if cfg.locked_ip.is_empty() {
                    "not pinned"
                } else {
                    &cfg.locked_ip
                },
            );
            let active_cfg = running
                .as_ref()
                .and_then(|i| i.config.as_ref())
                .unwrap_or(&cfg);
            if active_cfg != &cfg {
                ui::warning(
                    "Active configuration differs; run 'ippin service restart' to apply saved changes.",
                );
            }
            let pinned = active_cfg.pinned_ip()?;
            if active_cfg.locked_ip != cfg.locked_ip {
                ui::field("Active pinned IP", pinned);
            }
            let own_port = running.as_ref().map_or(cfg.port, |i| i.port);
            let route = if let Some(instance) = &running {
                // Probe the active route, not a fresh auto-discovery decision.
                transport::resolve(&instance.route, 0).await?
            } else {
                transport::resolve(&cfg.upstream_proxy, cfg.port).await?
            };
            match Checker::new(
                Transport::new(route, pinned.is_ipv6(), own_port),
                pinned.is_ipv6(),
            )
            .fetch()
            .await
            {
                Ok(current) => {
                    ui::field(
                        "Current IP",
                        format!("{} ({})", current.ip, current.country),
                    );
                    if current.ip == pinned {
                        ui::state("IP check", "MATCH", "", Tone::Success);
                    } else {
                        ui::state("IP check", "BLOCKED", "mismatch", Tone::Danger);
                    }
                }
                Err(e) => {
                    ui::state("IP check", "BLOCKED", "unreachable", Tone::Danger);
                    ui::section("Routing");
                    ui::field("Protected domains", cfg.domains.join(", "));
                    return Err(e);
                }
            }
            ui::section("Routing");
            ui::field("Protected domains", cfg.domains.join(", "));
            ui::field("Local gate", format!("http://127.0.0.1:{own_port}"));
            ui::field("PAC", format!("http://127.0.0.1:{own_port}/proxy.pac"));
            if running.is_none() {
                ui::warning(
                    "The proxy is stopped. Start it with 'ippin start' or 'ippin service start'.",
                );
            }
            Ok(())
        }
        Command::Service { action } => {
            ui::title("Background process");
            match action {
                ServiceAction::Install => {
                    store.load()?.pinned_ip()?;
                    platform::install_autostart(&store)?;
                    match service::start(&store, true).await {
                        Ok(instance) => ui::success(format!(
                            "Windows login autostart installed. Process ready (PID {}).",
                            instance.pid
                        )),
                        Err(e) => {
                            platform::uninstall_autostart(&store)?;
                            return Err(e);
                        }
                    }
                }
                ServiceAction::Uninstall => {
                    service::stop(&store).await?;
                    platform::uninstall_autostart(&store)?;
                    ui::success("Autostart removed; process stopped.");
                }
                ServiceAction::Stop => {
                    service::stop(&store).await?;
                    ui::success("Process stopped.");
                }
                ServiceAction::Start | ServiceAction::Restart => {
                    let cfg = store.load()?;
                    cfg.pinned_ip()?;
                    if matches!(action, ServiceAction::Restart) {
                        service::stop(&store).await?;
                    }
                    let instance =
                        service::start(&store, cfg!(windows) && cfg.auto_win_pac).await?;
                    ui::success(format!(
                        "Process ready (PID {}, port {}).",
                        instance.pid, instance.port
                    ));
                    if ui::interactive() {
                        ui::hint("View events with 'ippin logs -n 50'.");
                    }
                }
                ServiceAction::Status => match service::running(&store).await? {
                    Some(instance) => {
                        ui::state(
                            "Process",
                            "RUNNING",
                            format!("PID {}, port {}", instance.pid, instance.port),
                            Tone::Success,
                        );
                    }
                    None => ui::state("Process", "STOPPED", "", Tone::Warning),
                },
            }
            Ok(())
        }
        Command::Logs { lines } => logs(&store, lines as usize),
        Command::Win { action } => {
            ui::title("Windows PAC");
            match action {
                WinAction::Enable => {
                    let instance = service::running(&store)
                        .await?
                        .context("start IPPin before enabling Windows PAC")?;
                    platform::enable_pac(&store, instance.port)?;
                }
                WinAction::Disable => platform::disable_pac(&store)?,
            }
            ui::success("Windows PAC settings updated.");
            Ok(())
        }
    }
}

async fn changed(store: &Store) -> Result<()> {
    if service::running(store).await?.is_some() {
        ui::warning(
            "Running process uses its startup configuration. Apply changes with 'ippin service restart'.",
        );
    }
    Ok(())
}

async fn start(store: &Store, args: StartArgs) -> Result<()> {
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("ippin.log")
        .max_log_files(7)
        .build(&store.dir)?;
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let console = (!args.headless && ui::interactive()).then(|| {
        tracing_subscriber::fmt::layer()
            .compact()
            .with_target(false)
            .with_ansi(ui::colors_enabled())
            .with_writer(std::io::stderr)
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .with(console)
        .try_init()
        .map_err(|e| anyhow::anyhow!("initialize logging: {e}"))?;
    let result = start_runtime(store, args).await;
    if let Err(e) = &result {
        tracing::error!(error = %format!("{e:#}"), "server failed");
    }
    result
}

async fn start_runtime(store: &Store, args: StartArgs) -> Result<()> {
    if !args.headless {
        ui::title("Local gatekeeper");
    }
    let _lock = store.lock("instance.lock", true)?;
    let mut cfg = store.load()?;
    if let Some(port) = args.port {
        cfg.port = port;
    }
    cfg.validate()?;
    let pinned = cfg.pinned_ip()?;
    // Bind before touching system settings or advertising process readiness.
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, cfg.port))
        .await
        .context("bind local proxy")?;
    let route = transport::resolve(&cfg.upstream_proxy, cfg.port).await?;
    let transport = Transport::new(route, pinned.is_ipv6(), cfg.port);
    let shutdown = CancellationToken::new();
    let gate = Arc::new(Gate::new(
        pinned,
        Arc::new(Checker::new(transport.clone(), pinned.is_ipv6())),
        shutdown.clone(),
    ));
    let _pac = if args.win_pac {
        Some(platform::PacGuard::enable(store, cfg.port)?)
    } else {
        None
    };
    let mut instance = Instance::new(cfg.port);
    instance.route = transport.route.description();
    instance.config = Some(cfg.clone());
    instance.save(store)?;
    struct StateCleanup<'a>(&'a Store);
    impl Drop for StateCleanup<'_> {
        fn drop(&mut self) {
            if let Err(e) = fs::remove_file(self.0.path("instance.json")) {
                tracing::warn!(error = %e, "remove instance state");
            }
        }
    }
    let _state = StateCleanup(store);
    let cancel = shutdown.clone();
    let signal = tokio::spawn(async move {
        shutdown_signal().await;
        cancel.cancel();
    });
    if !args.headless {
        ui::state("Process", "RUNNING", "", Tone::Success);
        ui::field("Pinned IP", pinned);
        ui::field("Route", transport.route.description());
        ui::section("Local endpoints");
        ui::field("HTTP / HTTPS", format!("http://127.0.0.1:{}", cfg.port));
        ui::field("PAC", format!("http://127.0.0.1:{}/proxy.pac", cfg.port));
        ui::field(
            "Anthropic API",
            format!("http://127.0.0.1:{}/anthropic", cfg.port),
        );
        ui::section("Protected domains");
        ui::field("Domains", cfg.domains.join(", "));
        ui::hint("Ctrl+C to stop · ippin logs -n 50 to view events");
        ui::section("Live events");
    }
    tracing::info!(port = cfg.port, route = %transport.route.description(), "IPPin ready");
    let result = Server::new(cfg, gate, transport, instance, shutdown)
        .serve(listener)
        .await;
    signal.abort();
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn logs(store: &Store, lines: usize) -> Result<()> {
    ui::title("Recent events");
    let mut paths: Vec<_> = fs::read_dir(&store.dir)?
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("ippin.log"))
        })
        .collect();
    paths.sort();
    let mut tail = VecDeque::with_capacity(lines);
    for path in paths {
        for line in BufReader::new(fs::File::open(path)?).lines() {
            if tail.len() == lines {
                tail.pop_front();
            }
            tail.push_back(line?);
        }
    }
    if tail.is_empty() {
        ui::hint("No log records yet.");
    }
    for line in tail {
        ui::log_line(&line);
    }
    Ok(())
}
