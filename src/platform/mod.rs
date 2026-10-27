//! OS integration is isolated from admission policy and network transport.
use crate::config::Store;
use anyhow::Result;

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{disable_pac, enable_pac, install_autostart, uninstall_autostart};

#[cfg(windows)]
pub use windows::spawn_background;
#[cfg(not(windows))]
pub fn spawn_background(mut command: std::process::Command) -> Result<std::process::Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Ok(command.spawn()?)
}

#[cfg(not(windows))]
pub fn enable_pac(_: &Store, _: u16) -> Result<()> {
    anyhow::bail!("system PAC integration is supported only on Windows")
}
#[cfg(not(windows))]
pub fn disable_pac(_: &Store) -> Result<()> {
    anyhow::bail!("system PAC integration is supported only on Windows")
}
#[cfg(not(windows))]
pub fn install_autostart(_: &Store) -> Result<()> {
    anyhow::bail!(
        "autostart installation is supported only on Windows; use your OS service manager"
    )
}
#[cfg(not(windows))]
pub fn uninstall_autostart(_: &Store) -> Result<()> {
    anyhow::bail!("autostart installation is supported only on Windows")
}

/// A successful enable always has a matching restore, including server errors.
pub struct PacGuard<'a> {
    store: &'a Store,
}
impl<'a> PacGuard<'a> {
    pub fn enable(store: &'a Store, port: u16) -> Result<Self> {
        enable_pac(store, port)?;
        Ok(Self { store })
    }
}
impl Drop for PacGuard<'_> {
    fn drop(&mut self) {
        if let Err(e) = disable_pac(self.store) {
            tracing::error!(error = %e, "failed to restore Windows PAC; run 'ippin win disable'");
        }
    }
}
