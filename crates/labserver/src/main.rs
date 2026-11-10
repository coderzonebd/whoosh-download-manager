//! `vayu-labserver` command-line entry point.

use anyhow::Context;
use clap::Parser;
use std::net::IpAddr;
use std::path::PathBuf;

use vayu_labserver::{LabConfig, LabServer, Scenario, scenario};

/// Deterministic, fault-injecting HTTP test server for vayu's tests and
/// benchmarks.
#[derive(Debug, Parser)]
#[command(name = "vayu-labserver", version)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1")]
    bind: IpAddr,
    /// Port for HTTP/1.1 and h2c (prior knowledge). 0 picks a free port.
    #[arg(long, default_value_t = 18080)]
    port: u16,
    /// Port for HTTPS (ALPN h2 + http/1.1, self-signed certificate).
    #[arg(long, default_value_t = 18443)]
    tls_port: u16,
    /// Do not start the HTTPS listener.
    #[arg(long)]
    no_tls: bool,
    /// Serve HTTP/3 (QUIC) on this UDP port and advertise it with Alt-Svc.
    /// Without a value (or 0): the HTTPS port's number (any free port with
    /// --no-tls).
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "0")]
    h3_port: Option<u16>,
    /// Extra scenario directory (*.toml); same names override built-ins.
    #[arg(long)]
    scenarios: Option<PathBuf>,
    /// Serve only scenarios from --scenarios, not the built-in set.
    #[arg(long, requires = "scenarios")]
    no_builtin: bool,
    /// Memory for cached content blocks, in MiB.
    #[arg(long, default_value_t = 256)]
    cache_mb: usize,
    /// Write the self-signed certificate (PEM) here, for curl --cacert etc.
    #[arg(long)]
    write_cert: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "vayu_labserver=info".into()),
        )
        .init();
    let args = Args::parse();

    let mut scenarios: Vec<Scenario> = if args.no_builtin {
        Vec::new()
    } else {
        scenario::builtin()
    };
    if let Some(dir) = &args.scenarios {
        for sc in scenario::load_dir(dir)? {
            scenarios.retain(|s| s.name != sc.name);
            scenarios.push(sc);
        }
    }
    let config = LabConfig {
        bind: args.bind,
        http_port: args.port,
        tls_port: (!args.no_tls).then_some(args.tls_port),
        h3_port: args.h3_port,
        scenarios,
        cache_bytes: args.cache_mb << 20,
        ..LabConfig::default()
    };
    let lab = LabServer::start(config).await?;

    if let (Some(path), Some(pem)) = (&args.write_cert, lab.cert_pem()) {
        std::fs::write(path, pem).with_context(|| format!("writing {}", path.display()))?;
    }

    println!("vayu-labserver listening");
    println!(
        "  http   http://{}  (HTTP/1.1, h2c prior knowledge)",
        lab.http_addr()
    );
    if let Some(a) = lab.https_addr() {
        println!(
            "  https  https://localhost:{}  (ALPN h2, http/1.1; cert at /_lab/cert.pem)",
            a.port()
        );
    }
    if let Some(a) = lab.h3_addr() {
        println!(
            "  h3     https://localhost:{}  (HTTP/3 over QUIC, UDP; Alt-Svc on the TCP listeners)",
            a.port()
        );
    }
    println!("scenarios:");
    for name in lab.scenario_names() {
        let desc = lab
            .scenario(&name)
            .map(|s| s.description.as_str())
            .unwrap_or("");
        println!("  {name:<18} {desc}");
    }
    println!("try: curl -o /dev/null {}", lab.url("clean", "100m.bin"));

    tokio::signal::ctrl_c().await?;
    println!("shutting down");
    lab.shutdown().await;
    Ok(())
}
