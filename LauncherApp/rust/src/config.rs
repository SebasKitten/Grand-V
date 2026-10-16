use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub port: u16,
    pub mode: String,
    pub max_clients: usize,
    pub max_connections: usize,
    pub speed_mib: f64,
    pub https: bool,
    pub tls_cert: String,
    pub tls_key: String,
    pub relay_port: u16,
    pub public_origin: String,
    pub cloudflare_tunnel: bool,
    pub multiplayer: bool,
    // Preserve settings written by newer launchers.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            port: 8000,
            mode: "device".into(),
            max_clients: 0,
            max_connections: 0,
            speed_mib: 0.,
            https: false,
            tls_cert: String::new(),
            tls_key: String::new(),
            relay_port: 8012,
            public_origin: String::new(),
            cloudflare_tunnel: false,
            multiplayer: false,
            extra: Default::default(),
        }
    }
}
impl Settings {
    pub fn connection_limit(&self) -> usize {
        if self.cloudflare_tunnel && self.mode == "external" {
            200
        } else {
            self.max_connections
        }
    }
    pub fn load(folder: &Path) -> Result<Self> {
        let path = folder.join("launcher-settings.json");
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
    pub fn save(&self, folder: &Path) -> Result<()> {
        crate::storage::atomic_write(
            &folder.join("launcher-settings.json"),
            &serde_json::to_vec_pretty(self)?,
            false,
        )
    }
    pub fn validate(&self) -> Result<()> {
        if self.port < 1024 || self.relay_port < 1024 {
            bail!("Ports must be 1024–65535");
        }
        if !["device", "lan", "external"].contains(&self.mode.as_str()) {
            bail!("Invalid audience");
        }
        if self.max_clients > 10000 || !self.speed_mib.is_finite() || self.speed_mib < 0. {
            bail!("Invalid client/bandwidth limit");
        }
        if self.https && (self.tls_cert.is_empty() || self.tls_key.is_empty()) {
            bail!("HTTPS requires certificate and key PEM files");
        }
        if self.mode == "external" {
            origin(&self.public_origin, true)?;
            if self.https
                && Path::new(&self.tls_cert)
                    .file_name()
                    .is_some_and(|p| p == "lan-server.pem")
            {
                bail!("Private LAN certificates cannot be used for public websites");
            }
        }
        Ok(())
    }
    pub fn bind_ip(&self) -> Ipv4Addr {
        if self.mode == "device" || (self.mode == "external" && !self.https) {
            Ipv4Addr::LOCALHOST
        } else {
            Ipv4Addr::UNSPECIFIED
        }
    }
    pub fn scheme(&self) -> &'static str {
        if self.https { "https" } else { "http" }
    }
}
pub fn resolve_folder(path: &Path) -> Result<PathBuf> {
    for candidate in [
        path.to_path_buf(),
        path.join("GrandV-assets"),
        path.join("playgta5-offline"),
        path.parent()
            .and_then(Path::parent)
            .unwrap_or(path)
            .to_path_buf(),
    ] {
        if candidate
            .join("mirror/playgta5.com/launcher.html")
            .is_file()
        {
            return Ok(candidate.canonicalize()?);
        }
    }
    bail!("Select the extracted GrandV-assets folder, not the launcher folder or an archive. Expected mirror/playgta5.com/launcher.html inside it.")
}
/// Check release contents once, before hosting. Legacy development folders remain supported.
pub fn validate_asset_package(folder: &Path) -> Result<()> {
    let marker = folder.join("grandv-assets.json");
    if !marker.is_file() {
        return Ok(());
    }
    let package: serde_json::Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
    if package["format"] != "grandv-assets-v1" {
        bail!("Unsupported GrandV-assets package. Extract the current asset download.");
    }
    let entries = package["files"].as_array().ok_or_else(|| {
        anyhow::anyhow!("GrandV-assets file list is missing. Extract the complete package again.")
    })?;
    if entries.is_empty() {
        bail!("GrandV-assets file list is empty. Extract the complete package again.");
    }
    for entry in entries {
        let relative = entry[0]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid asset file entry"))?;
        let expected = entry[1]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid asset size"))?;
        let path = Path::new(relative);
        if path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            bail!("Invalid GrandV-assets path: {relative}");
        }
        match std::fs::metadata(folder.join(path)) {
            Ok(meta) if meta.is_file() && meta.len() == expected => {}
            _ => bail!(
                "GrandV-assets is incomplete: {relative} is missing or has the wrong size. Finish downloading and extract the complete folder again."
            ),
        }
    }
    Ok(())
}
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
pub fn access_code(value: &str) -> Result<String> {
    let value = value.trim();
    if !(4..=128).contains(&value.len()) || !value.bytes().all(|b| (32..=126).contains(&b)) {
        bail!("Access code must contain 4–128 printable ASCII characters");
    }
    Ok(value.to_owned())
}
pub fn load_code(folder: &Path) -> Result<String> {
    let file = folder.join("multiplayer-access-code.txt");
    if file.exists() {
        access_code(&std::fs::read_to_string(file)?)
    } else {
        Ok(String::new())
    }
}
pub fn save_code(folder: &Path, requested: &str) -> Result<String> {
    let code = if requested.trim().is_empty() {
        let saved = load_code(folder)?;
        if saved.is_empty() {
            random_token()
        } else {
            saved
        }
    } else {
        access_code(requested)?
    };
    crate::storage::atomic_write(
        &folder.join("multiplayer-access-code.txt"),
        code.as_bytes(),
        true,
    )?;
    Ok(code)
}
pub fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback(),
    }
}
pub fn addresses() -> Vec<Ipv4Addr> {
    let mut ips: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
            _ => None,
        })
        .collect();
    ips.sort();
    ips.dedup();
    ips
}
pub fn origin(value: &str, https: bool) -> Result<String> {
    if value.chars().any(char::is_control) {
        bail!("Invalid origin");
    }
    let u = url::Url::parse(value)?;
    if (https && u.scheme() != "https")
        || !["http", "https"].contains(&u.scheme())
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.path() != "/"
        || u.query().is_some()
        || u.fragment().is_some()
        || u.port() == Some(0)
    {
        bail!("Enter an HTTPS origin without a path or credentials, e.g. https://example.com");
    }
    Ok(u.origin().ascii_serialization())
}
pub fn join_url(value: &str) -> Result<String> {
    if value.chars().any(char::is_control) {
        bail!("Invalid website URL");
    }
    let mut u = url::Url::parse(value.trim())?;
    if !["http", "https"].contains(&u.scheme())
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port() == Some(0)
    {
        bail!("Enter an http:// or https:// website URL");
    }
    if u.path() == "/" {
        u.set_path("/launcher.html");
    }
    Ok(u.into())
}
