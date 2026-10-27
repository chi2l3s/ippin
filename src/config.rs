use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub locked_ip: String,
    pub locked_country: String,
    pub port: u16,
    pub upstream_proxy: String,
    pub auto_win_pac: bool,
    pub domains: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            locked_ip: String::new(),
            locked_country: String::new(),
            port: 8888,
            upstream_proxy: "auto".into(),
            auto_win_pac: true,
            domains: preset("claude")
                .expect("built-in preset")
                .iter()
                .map(|s| (*s).into())
                .collect(),
        }
    }
}

impl Config {
    pub fn validate(&mut self) -> Result<()> {
        ensure!(self.port != 0, "port must be between 1 and 65535");
        if !self.locked_ip.is_empty() {
            self.locked_ip = self
                .locked_ip
                .parse::<IpAddr>()
                .context("invalid locked_ip")?
                .to_string();
        }
        self.upstream_proxy = crate::transport::normalize_mode(&self.upstream_proxy, self.port)?;
        let mut domains = Vec::new();
        for domain in &self.domains {
            let domain = normalize_domain(domain)?;
            if !domains.contains(&domain) {
                domains.push(domain);
            }
        }
        self.domains = domains;
        Ok(())
    }

    pub fn pinned_ip(&self) -> Result<IpAddr> {
        self.locked_ip
            .parse()
            .context("No valid IP pinned. Enable your VPN and run 'ippin lock' first")
    }

    pub fn add_domain(&mut self, domain: &str) -> Result<bool> {
        let domain = normalize_domain(domain)?;
        if self.domains.contains(&domain) {
            return Ok(false);
        }
        self.domains.push(domain);
        Ok(true)
    }
}

pub fn normalize_domain(raw: &str) -> Result<String> {
    let domain = raw
        .trim()
        .trim_start_matches("*.")
        .trim_end_matches('.')
        .to_ascii_lowercase();
    ensure!(
        !domain.is_empty() && domain.len() <= 253,
        "invalid domain: {raw}"
    );
    ensure!(
        domain.parse::<IpAddr>().is_err(),
        "use a DNS domain, not an IP address: {raw}"
    );
    for label in domain.split('.') {
        ensure!(
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
            "invalid domain: {raw}"
        );
    }
    ensure!(
        domain.contains('.'),
        "domain must contain at least two labels: {raw}"
    );
    Ok(domain)
}

pub fn preset(name: &str) -> Result<&'static [&'static str]> {
    match name.to_ascii_lowercase().as_str() {
        "claude" => Ok(&["claude.ai", "anthropic.com", "claude.com"]),
        "openai" => Ok(&[
            "chatgpt.com",
            "openai.com",
            "oaistatic.com",
            "oaiusercontent.com",
        ]),
        "gemini" => Ok(&[
            "gemini.google.com",
            "aistudio.google.com",
            "generativelanguage.googleapis.com",
        ]),
        _ => bail!("unknown preset '{name}'; available: claude, openai, gemini"),
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Result<Self> {
        ensure!(
            !dir.as_os_str().is_empty(),
            "state directory cannot be empty"
        );
        let dir = std::path::absolute(dir).context("resolve state directory")?;
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir })
    }

    pub fn default_path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("IPPIN_HOME") {
            return Ok(path.into());
        }
        Ok(dirs::home_dir()
            .context("home directory unavailable; set IPPIN_HOME")?
            .join(".ippin"))
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    pub fn load(&self) -> Result<Config> {
        let path = self.path("config.json");
        let mut cfg: Config = match fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data)
                .with_context(|| format!("invalid JSON in {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).context("read config"),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    /// A cross-process lock and atomic replacement prevent torn/lost CLI updates.
    pub fn update(&self, change: impl FnOnce(&mut Config) -> Result<()>) -> Result<Config> {
        let _lock = self.lock("config.lock", false)?;
        let mut cfg = self.load()?;
        change(&mut cfg)?;
        cfg.validate()?;
        atomic_json(&self.path("config.json"), &cfg)?;
        Ok(cfg)
    }

    pub fn lock(&self, name: &str, exclusive_instance: bool) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path(name))?;
        if exclusive_instance {
            file.try_lock_exclusive()
                .context("another IPPin instance is already running")?;
        } else {
            file.lock_exclusive()?;
        }
        Ok(file)
    }
}

pub fn atomic_json(path: &Path, data: &impl Serialize) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("path has no parent")?)?;
    serde_json::to_writer_pretty(&mut tmp, data)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| e.error)
        .context("replace state file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_normalized_and_reject_pac_injection() {
        assert_eq!(normalize_domain(" *.Claude.AI. ").unwrap(), "claude.ai");
        for bad in [
            "",
            "x\"); return 'DIRECT'",
            "https://claude.ai",
            "-bad.ai",
            "a..com",
            "127.0.0.1",
            "localhost",
            "a/b.com",
        ] {
            assert!(normalize_domain(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn old_go_config_is_compatible_and_updates_are_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().into()).unwrap();
        fs::write(store.path("config.json"), r#"{"locked_ip":"203.0.113.1","port":8888,"upstream_proxy":"tun","domains":["CLAUDE.AI"]}"#).unwrap();
        let cfg = store
            .update(|c| {
                c.add_domain("*.openai.com")?;
                Ok(())
            })
            .unwrap();
        assert_eq!(cfg.locked_ip, "203.0.113.1");
        assert_eq!(store.load().unwrap(), cfg);
        assert_eq!(cfg.domains, ["claude.ai", "openai.com"]);
        assert!(
            store
                .update(|c| {
                    c.port = 0;
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(store.load().unwrap(), cfg);
    }

    #[test]
    fn corrupt_config_is_never_replaced_by_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().into()).unwrap();
        fs::write(store.path("config.json"), "{").unwrap();
        assert!(store.update(|_| Ok(())).is_err());
        assert_eq!(fs::read_to_string(store.path("config.json")).unwrap(), "{");
    }
}
