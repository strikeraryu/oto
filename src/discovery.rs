use crate::protocol::{SERVICE, VERSION};
use anyhow::{bail, Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use uuid::Uuid;

pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}
impl Advertisement {
    pub fn publish(code: Option<&str>, session: Uuid, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let name = format!("oto-{}", session.simple());
        let mut properties = HashMap::from([
            ("session".into(), session.to_string()),
            ("version".into(), VERSION.to_string()),
        ]);
        if let Some(code) = code {
            properties.insert("code".into(), code.to_string());
        }
        let info = ServiceInfo::new(
            SERVICE,
            &name,
            // A private alias avoids claiming macOS's own Bonjour hostname.
            &format!("{name}.local."),
            "",
            port,
            properties,
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        Ok(Self { daemon, fullname })
    }
}
impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

pub async fn find(code: &str, timeout: Duration) -> Result<SocketAddr> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE)?;
    let result = tokio::time::timeout(timeout, async {
        loop {
            match receiver
                .recv_async()
                .await
                .context("Bonjour discovery stopped")?
            {
                ServiceEvent::ServiceResolved(info)
                    if info.get_property_val_str("code") == Some(code) =>
                {
                    if info.get_property_val_str("version") != Some(&VERSION.to_string()) {
                        continue;
                    }
                    let mut addresses = info.get_addresses().iter().copied().collect::<Vec<_>>();
                    addresses.sort_by_key(|address| match address {
                        IpAddr::V4(ip) if !ip.is_loopback() => 0,
                        IpAddr::V4(_) => 1,
                        IpAddr::V6(_) => 2,
                    });
                    if let Some(address) = addresses
                        .into_iter()
                        .find(|ip| !matches!(ip, IpAddr::V6(v6) if v6.is_unicast_link_local()))
                    {
                        return Ok(SocketAddr::new(address, info.get_port()));
                    }
                }
                _ => {}
            }
        }
    })
    .await;
    let _ = daemon.stop_browse(SERVICE);
    let _ = daemon.shutdown();
    match result { Ok(result) => result, Err(_) => bail!("No host found for {code}. Check the code, local network and firewall, or use --host IP:PORT.") }
}
