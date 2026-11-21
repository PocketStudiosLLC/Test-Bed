//! Trust stores: which clients a host accepts, and which hosts a client knows.
//! PCs paired over the internet (spec3) live in their own files, keyed by
//! their iroh endpoint id where LAN PCs have a certificate fingerprint.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{config_dir, write_private};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct AuthorizedClient {
    pub name: String,
    pub fingerprint: String,
    pub paired_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct KnownHost {
    pub name: String,
    /// "ip:port" or "hostname:port" used to reach the host. For an internet
    /// host, its relay URL (empty: look it up).
    pub address: String,
    /// Certificate fingerprint, or an internet host's endpoint id.
    pub fingerprint: String,
    pub paired_at: u64,
    /// Unix time of the last successful connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<u64>,
    /// Reached over the internet (from `internet_hosts.toml`).
    #[serde(skip)]
    pub internet: bool,
    /// A LAN host that also lets this client in over the internet (linked
    /// while connected on the LAN): used when the LAN can't reach it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<InternetLink>,
}

/// How to reach a LAN host over the internet.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InternetLink {
    /// The host's iroh endpoint id.
    pub id: String,
    /// Its relay URL when linked (empty: look it up).
    #[serde(default)]
    pub relay: String,
}

impl KnownHost {
    /// The same host as an internet one, if it is linked.
    pub fn over_internet(&self) -> Option<KnownHost> {
        let l = self.link.as_ref()?;
        Some(KnownHost { address: l.relay.clone(), fingerprint: l.id.clone(), internet: true, link: None, ..self.clone() })
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct AuthorizedClients {
    #[serde(default, rename = "client")]
    pub clients: Vec<AuthorizedClient>,
    #[serde(skip)]
    internet: bool,
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct KnownHosts {
    #[serde(default, rename = "host")]
    pub hosts: Vec<KnownHost>,
    #[serde(skip)]
    internet: bool,
}

fn load<T: Default + for<'de> Deserialize<'de>>(path: &PathBuf) -> Result<T> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn save<T: Serialize>(path: &PathBuf, v: &T) -> Result<()> {
    write_private(path, toml::to_string_pretty(v)?.as_bytes(), 0o600)
}

impl AuthorizedClients {
    pub fn path() -> PathBuf {
        config_dir().join("authorized_clients.toml")
    }
    pub fn internet_path() -> PathBuf {
        config_dir().join("internet_clients.toml")
    }
    pub fn load() -> Result<Self> {
        load(&Self::path())
    }
    /// Clients paired over the internet; `fingerprint` is their endpoint id.
    pub fn load_internet() -> Result<Self> {
        Ok(Self { internet: true, ..load(&Self::internet_path())? })
    }
    pub fn save(&self) -> Result<()> {
        save(&if self.internet { Self::internet_path() } else { Self::path() }, self)
    }
    pub fn find_fingerprint(&self, fp: &str) -> Option<&AuthorizedClient> {
        self.clients.iter().find(|c| c.fingerprint == fp)
    }
    /// Add or replace (by fingerprint, then by name).
    pub fn upsert(&mut self, c: AuthorizedClient) {
        self.clients.retain(|o| o.fingerprint != c.fingerprint && o.name != c.name);
        self.clients.push(c);
    }
    /// Remove by name or fingerprint prefix; returns whether anything was removed.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.clients.len();
        self.clients.retain(|c| c.name != key && !(key.len() >= 8 && c.fingerprint.starts_with(key)));
        before != self.clients.len()
    }
}

impl KnownHosts {
    pub fn path() -> PathBuf {
        config_dir().join("known_hosts.toml")
    }
    pub fn internet_path() -> PathBuf {
        config_dir().join("internet_hosts.toml")
    }
    pub fn load() -> Result<Self> {
        load(&Self::path())
    }
    /// Hosts paired over the internet; `fingerprint` is their endpoint id.
    pub fn load_internet() -> Result<Self> {
        let mut kh: Self = load(&Self::internet_path())?;
        kh.internet = true;
        for h in &mut kh.hosts {
            h.internet = true;
        }
        Ok(kh)
    }
    pub fn save(&self) -> Result<()> {
        save(&if self.internet { Self::internet_path() } else { Self::path() }, self)
    }
    pub fn find(&self, name: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.name == name).or_else(|| self.hosts.iter().find(|h| h.address == name))
    }
    pub fn upsert(&mut self, h: KnownHost) {
        self.hosts.retain(|o| o.fingerprint != h.fingerprint && o.name != h.name);
        self.hosts.push(h);
    }
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| h.name != key && h.address != key);
        before != self.hosts.len()
    }
    /// Record a successful connection.
    pub fn touch(&mut self, fingerprint: &str) {
        if let Some(h) = self.hosts.iter_mut().find(|h| h.fingerprint == fingerprint) {
            h.last_seen = Some(crate::unix_now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_survive_saving_and_old_files_load() {
        let h = KnownHost {
            name: "desk".into(),
            address: "192.168.1.5:9876".into(),
            fingerprint: "ab".repeat(32),
            paired_at: 1,
            last_seen: Some(2),
            internet: false,
            link: Some(InternetLink { id: "cd".repeat(32), relay: "https://relay.example/".into() }),
        };
        let kh = KnownHosts { hosts: vec![h.clone()], internet: false };
        let back: KnownHosts = toml::from_str(&toml::to_string_pretty(&kh).unwrap()).unwrap();
        assert_eq!(back.hosts, vec![h.clone()]);
        let away = h.over_internet().unwrap();
        assert!(away.internet && away.link.is_none());
        assert_eq!((away.fingerprint.as_str(), away.address.as_str()), (&*"cd".repeat(32), "https://relay.example/"));
        let old: KnownHosts = toml::from_str("[[host]]\nname = \"a\"\naddress = \"x:1\"\nfingerprint = \"f\"\npaired_at = 0\n").unwrap();
        assert!(old.hosts[0].link.is_none() && old.hosts[0].over_internet().is_none());
    }
}
