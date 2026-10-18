use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    net::{Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};
use tokio::{net::UdpSocket, sync::watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Found {
    pub name: String,
    pub url: String,
    pub fingerprint: String,
}
pub async fn advertise(
    socket: UdpSocket,
    mut port: watch::Receiver<u16>,
    https: bool,
    fingerprint: String,
    stop: CancellationToken,
) -> Result<()> {
    let mut buffer = [0u8; 513];
    let mut seen: HashMap<std::net::IpAddr, Instant> = HashMap::new();
    loop {
        tokio::select! {
            _=stop.cancelled()=>return Ok(()),
            _=port.changed()=>{},
            packet=socket.recv_from(&mut buffer)=>{
                let (len,peer)=packet?; if len>512 || !crate::config::private(peer.ip()) {continue;}
                let Ok(v)=serde_json::from_slice::<Value>(&buffer[..len]) else {continue;};
                let Some(nonce)=v["nonce"].as_str().filter(|s|(16..=128).contains(&s.len())) else {continue;};
                if v["marker"]!="GTA5LAN1" {continue;}
                let now=Instant::now(); seen.retain(|_,t|now.duration_since(*t)<Duration::from_secs(5));
                if seen.get(&peer.ip()).is_some_and(|t|now.duration_since(*t)<Duration::from_millis(500)) || (!seen.contains_key(&peer.ip()) && seen.len()>=256) {continue;}
                seen.insert(peer.ip(),now);
                let msg=json!({"marker":"GTA5LAN1","nonce":nonce,"port":*port.borrow(),"https":https,"name":crate::APP_NAME,"fingerprint":fingerprint}).to_string();
                if msg.len()<=1024 {let _=socket.send_to(msg.as_bytes(),peer).await;}
            }
        }
    }
}
pub async fn discover() -> Result<Vec<Found>> {
    let mut bindings = vec![Ipv4Addr::UNSPECIFIED];
    bindings.extend(crate::config::addresses().into_iter().take(8));
    let nonce = crate::config::random_token();
    let message = json!({"marker":"GTA5LAN1","nonce":nonce}).to_string();
    let mut sockets = Vec::new();
    for ip in bindings {
        let Ok(socket) = UdpSocket::bind((ip, 0)).await else {
            continue;
        };
        socket.set_broadcast(true)?;
        if socket
            .send_to(message.as_bytes(), "255.255.255.255:8014")
            .await
            .is_ok()
        {
            sockets.push(socket);
        }
    }
    anyhow::ensure!(!sockets.is_empty(), "No discovery interface is available");
    let mut tasks = tokio::task::JoinSet::new();
    for socket in sockets {
        let nonce = nonce.clone();
        tasks.spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
            let mut buffer = [0u8; 1025];
            let mut rows = Vec::new();
            while let Ok(Ok((n, peer))) =
                tokio::time::timeout_at(deadline, socket.recv_from(&mut buffer)).await
            {
                if n > 1024 || !crate::config::private(peer.ip()) {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<Value>(&buffer[..n]) else {
                    continue;
                };
                let Some(port) = v["port"].as_u64().filter(|p| (1..=65535).contains(p)) else {
                    continue;
                };
                let Some(https) = v["https"].as_bool() else {
                    continue;
                };
                if v["marker"] != "GTA5LAN1" || v["nonce"] != nonce {
                    continue;
                }
                rows.push(Found {
                    name: v["name"].as_str().unwrap_or("").chars().take(80).collect(),
                    url: format!(
                        "{}://{}/launcher.html",
                        if https { "https" } else { "http" },
                        SocketAddr::new(peer.ip(), port as u16)
                    ),
                    fingerprint: v["fingerprint"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(128)
                        .collect(),
                });
                if rows.len() >= 32 {
                    break;
                }
            }
            rows
        });
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    while let Some(Ok(found)) = tasks.join_next().await {
        for row in found {
            if rows.len() < 32 && seen.insert(row.url.clone()) {
                rows.push(row);
            }
        }
    }
    rows.sort_by(|a, b| a.url.cmp(&b.url));
    Ok(rows)
}
