//! Bonjour advertising: `_mmserver._tcp` with `id`, `pv`, `name` and `auth` TXT records
//! (spec §3).

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use uuid::Uuid;

/// The service type.
pub const SERVICE_TYPE: &str = "_mmserver._tcp.local.";

/// What the advertisement says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advertisement {
    pub server_id: Uuid,
    pub name: String,
    pub auth: &'static str,
    pub port: u16,
}

/// A registered advertisement; dropping it withdraws it.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: Option<String>,
    current: Option<Advertisement>,
}

impl Advertiser {
    pub fn new() -> Result<Self> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
            fullname: None,
            current: None,
        })
    }

    /// Advertises, replacing any earlier advertisement whose details differ.
    pub fn advertise(&mut self, advertisement: &Advertisement) -> Result<()> {
        if self.current.as_ref() == Some(advertisement) {
            return Ok(());
        }
        self.withdraw();
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let host = host.strip_suffix(".local").unwrap_or(&host);
        let mut txt = HashMap::new();
        txt.insert("id".to_string(), advertisement.server_id.to_string());
        txt.insert("pv".to_string(), crate::http::PROTOCOL_MAJOR.to_string());
        txt.insert("name".to_string(), advertisement.name.clone());
        txt.insert("auth".to_string(), advertisement.auth.to_string());
        let instance = truncate_label(&advertisement.name);
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &format!("{host}.local."),
            "",
            advertisement.port,
            txt,
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        self.daemon.register(info)?;
        tracing::info!("advertising {fullname} on port {}", advertisement.port);
        self.fullname = Some(fullname);
        self.current = Some(advertisement.clone());
        Ok(())
    }

    /// Withdraws the advertisement.
    pub fn withdraw(&mut self) {
        if let Some(fullname) = self.fullname.take() {
            if let Ok(done) = self.daemon.unregister(&fullname) {
                let _ = done.recv_timeout(std::time::Duration::from_secs(1));
            }
        }
        self.current = None;
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.withdraw();
        let _ = self.daemon.shutdown();
    }
}

/// DNS labels are at most 63 bytes; cut on a character boundary.
fn truncate_label(name: &str) -> String {
    let name = if name.trim().is_empty() {
        "MiniMoon Media Server"
    } else {
        name
    };
    let mut end = name.len().min(63);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_names_on_a_character_boundary() {
        let long = "é".repeat(40);
        let label = truncate_label(&long);
        assert!(label.len() <= 63);
        assert!(long.starts_with(&label));
        assert_eq!(truncate_label(""), "MiniMoon Media Server");
    }
}
