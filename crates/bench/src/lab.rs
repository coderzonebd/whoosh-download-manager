//! The labserver the clients download from: started in-process, or an
//! existing one (`--lab-url`). Both are driven through labserver's
//! documented HTTP control API (`/_lab/...`), so a remote labserver on the
//! same host works exactly like the in-process one.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::time::{Duration, Instant};
use whoosh_labserver::{LabConfig, LabServer, ScenarioStats};

/// One file a scenario serves.
#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    #[serde(default)]
    pub content_disposition: Option<String>,
}

/// A scenario as `/_lab/scenarios` describes it.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioInfo {
    pub name: String,
    pub description: String,
    pub files: Vec<FileInfo>,
    #[serde(default)]
    pub config: serde_json::Value,
}

impl ScenarioInfo {
    pub fn file(&self, name: &str) -> Option<&FileInfo> {
        self.files.iter().find(|f| f.name == name)
    }

    /// Request headers a client must send (`[access]` cookie / referer).
    pub fn required_headers(&self) -> Vec<(String, String)> {
        let access = &self.config["access"];
        let mut h = Vec::new();
        if let Some(c) = access["cookie"].as_str() {
            h.push(("Cookie".to_owned(), c.to_owned()));
        }
        if let Some(r) = access["referer"].as_str() {
            h.push(("Referer".to_owned(), r.to_owned()));
        }
        h
    }

    /// Whether URLs must be signed (`[signed_url]`), i.e. minted per run.
    pub fn needs_signed_url(&self) -> bool {
        self.config.get("signed_url").is_some_and(|v| !v.is_null())
    }
}

#[derive(Debug, Deserialize)]
struct Digest {
    version: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct Minted {
    url: String,
}

/// A labserver to benchmark against.
#[derive(Debug)]
pub struct Lab {
    base: String,
    agent: ureq::Agent,
    scenarios: Vec<ScenarioInfo>,
    // Field order matters: the server must drop before its runtime.
    server: Option<LabServer>,
    _runtime: Option<tokio::runtime::Runtime>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .http_status_as_error(false)
        .build()
        .into()
}

impl Lab {
    /// Starts labserver in this process (HTTP only, ephemeral port, the
    /// built-in scenarios). `cache_mb` is its content block cache.
    pub fn start(cache_mb: usize) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("labserver")
            .build()?;
        let config = LabConfig {
            tls_port: None,
            cache_bytes: cache_mb << 20,
            ..LabConfig::default()
        };
        let server = runtime
            .block_on(LabServer::start(config))
            .context("start labserver")?;
        let base = format!("http://{}", server.http_addr());
        let mut lab = Self {
            base,
            agent: agent(),
            scenarios: Vec::new(),
            server: Some(server),
            _runtime: Some(runtime),
        };
        lab.scenarios = lab.fetch_scenarios()?;
        Ok(lab)
    }

    /// Uses a labserver that is already running at `url`
    /// (`http://127.0.0.1:18080`). It must be on this host for the
    /// time-to-first-byte metric, which compares clocks.
    pub fn connect(url: &str) -> Result<Self> {
        let mut lab = Self {
            base: url.trim_end_matches('/').to_owned(),
            agent: agent(),
            scenarios: Vec::new(),
            server: None,
            _runtime: None,
        };
        let health = lab.get_text("/_lab/health")?;
        if health.trim() != "ok" {
            bail!("{url}/_lab/health answered {health:?}");
        }
        lab.scenarios = lab.fetch_scenarios()?;
        Ok(lab)
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn in_process(&self) -> bool {
        self.server.is_some()
    }

    pub fn scenarios(&self) -> &[ScenarioInfo] {
        &self.scenarios
    }

    pub fn scenario(&self, name: &str) -> Option<&ScenarioInfo> {
        self.scenarios.iter().find(|s| s.name == name)
    }

    /// Plain URL of a file.
    pub fn url(&self, scenario: &str, file: &str) -> String {
        format!("{}/s/{scenario}/{file}", self.base)
    }

    /// The URL a client should be given for this run: freshly signed when
    /// the scenario needs it.
    pub fn run_url(&self, scenario: &str, file: &str) -> Result<String> {
        match self.scenario(scenario) {
            Some(sc) if sc.needs_signed_url() => {
                let m: Minted =
                    self.get_json(&format!("/_lab/mint?scenario={scenario}&file={file}"))?;
                Ok(m.url)
            }
            _ => Ok(self.url(scenario, file)),
        }
    }

    pub fn reset(&self, scenario: &str) -> Result<()> {
        let r = self
            .agent
            .post(&format!("{}/_lab/reset?scenario={scenario}", self.base))
            .send_empty()?;
        if r.status() != 200 {
            bail!("reset {scenario}: HTTP {}", r.status());
        }
        Ok(())
    }

    pub fn stats(&self, scenario: &str) -> Result<ScenarioStats> {
        self.get_json(&format!("/_lab/stats/{scenario}"))
    }

    /// Waits until no connection to `scenario` is open (the previous
    /// client's sockets are gone), so peaks measured after a reset belong
    /// to the next client alone. Gives up after `timeout`.
    pub fn wait_idle(&self, scenario: &str, timeout: Duration) -> Result<bool> {
        let t0 = Instant::now();
        loop {
            let s = self.stats(scenario)?;
            if s.open_connections == 0 && s.active_requests == 0 {
                return Ok(true);
            }
            if t0.elapsed() > timeout {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// SHA-256 (hex) of `version` of a file (`None` = the current version)
    /// and the version it belongs to.
    pub fn digest(
        &self,
        scenario: &str,
        file: &str,
        version: Option<u64>,
    ) -> Result<(u64, String)> {
        let mut path = format!("/_lab/digest?scenario={scenario}&file={file}");
        if let Some(v) = version {
            path.push_str(&format!("&version={v}"));
        }
        let d: Digest = self.get_json(&path)?;
        Ok((d.version, d.sha256))
    }

    fn fetch_scenarios(&self) -> Result<Vec<ScenarioInfo>> {
        self.get_json("/_lab/scenarios")
    }

    fn get_text(&self, path: &str) -> Result<String> {
        let mut r = self
            .agent
            .get(&format!("{}{path}", self.base))
            .call()
            .with_context(|| format!("GET {}{path}", self.base))?;
        if r.status() != 200 {
            bail!("GET {path}: HTTP {}", r.status());
        }
        Ok(r.body_mut().read_to_string()?)
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let s = self.get_text(path)?;
        serde_json::from_str(&s).with_context(|| format!("parse {path}"))
    }
}
