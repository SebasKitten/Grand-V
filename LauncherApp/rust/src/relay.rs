use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{Notify, Semaphore, mpsc};

#[derive(Clone)]
pub struct Relay {
    code: Arc<String>,
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    slots: Arc<Semaphore>,
}
struct Room {
    build: String,
    map: String,
    peers: HashMap<String, Peer>,
}
#[derive(Clone)]
struct Peer {
    events: mpsc::Sender<Value>,
    latest: Arc<Mutex<HashMap<String, Value>>>,
    wake: Arc<Notify>,
    stop: tokio_util::sync::CancellationToken,
}
impl Peer {
    fn event(&self, v: Value) {
        if self.events.try_send(v).is_err() {
            self.stop.cancel();
        }
        self.wake.notify_one();
    }
    fn pose(&self, v: Value) {
        self.latest
            .lock()
            .unwrap()
            .insert(v["id"].as_str().unwrap().to_string(), v);
        self.wake.notify_one();
    }
}
struct Membership {
    relay: Relay,
    room: String,
    id: String,
}
impl Drop for Membership {
    fn drop(&mut self) {
        crate::diagnostics::record(
            "Relay",
            format!("Player {} left room {}", self.id, self.room),
        );
        let mut rooms = self.relay.rooms.lock().unwrap();
        if let Some(room) = rooms.get_mut(&self.room) {
            room.peers.remove(&self.id);
            for peer in room.peers.values() {
                peer.latest.lock().unwrap().remove(&self.id);
                peer.event(json!({"type":"left","id":self.id}));
            }
            if room.peers.is_empty() {
                rooms.remove(&self.room);
            }
        }
    }
}
fn identifier(v: &Value, max: usize, dots: bool) -> bool {
    v.as_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= max
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || (dots && b == b'.'))
    })
}
pub fn snapshot(v: &Value) -> anyhow::Result<Value> {
    anyhow::ensure!(v["type"] == "pose", "Unsupported message");
    let seq = v["seq"]
        .as_u64()
        .filter(|n| *n <= 2147483647)
        .ok_or_else(|| anyhow::anyhow!("Invalid sequence"))?;
    let model = v["model"]
        .as_u64()
        .filter(|n| *n <= u32::MAX as u64)
        .ok_or_else(|| anyhow::anyhow!("Invalid model"))?;
    for (field, limit) in [("position", 20000.), ("rotation", 360.)] {
        anyhow::ensure!(
            v[field].as_array().is_some_and(|a| a.len() == 3
                && a.iter().all(|v| v
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.abs() <= limit))),
            "Invalid {field}"
        );
    }
    Ok(
        json!({"type":"pose","seq":seq,"model":model,"position":v["position"],"rotation":v["rotation"]}),
    )
}
async fn close(ws: &mut WebSocket, code: u16, reason: &str) {
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        ws.send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.to_string().into(),
        }))),
    )
    .await;
}
impl Relay {
    pub fn new(code: String) -> Self {
        Self {
            code: Arc::new(code),
            rooms: Default::default(),
            slots: Arc::new(Semaphore::new(64)),
        }
    }
    pub async fn handle(&self, mut ws: WebSocket) {
        let Ok(_slot) = self.slots.clone().try_acquire_owned() else {
            close(&mut ws, 1013, "Server full").await;
            return;
        };
        let hello = match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(s)))) => serde_json::from_str::<Value>(&s).ok(),
            _ => None,
        };
        let Some(hello) = hello else {
            close(&mut ws, 1008, "Invalid or incomplete request").await;
            return;
        };
        let checked = hello["type"] == "check";
        let error =
            if !(checked || hello["type"] == "join") || hello["protocol"].as_u64() != Some(1) {
                Some("Protocol mismatch")
            } else if !hello["key"]
                .as_str()
                .is_some_and(|s| bool::from(s.trim().as_bytes().ct_eq(self.code.as_bytes())))
            {
                Some("Join key rejected")
            } else if !identifier(&hello["room"], 32, false) {
                Some("Invalid room")
            } else if !identifier(&hello["build"], 96, true)
                || !["gta5", "env_test"].contains(&hello["map"].as_str().unwrap_or(""))
            {
                Some("Invalid build/map")
            } else {
                None
            };
        if let Some(error) = error {
            crate::diagnostics::record("Relay", format!("Request rejected: {error}"));
            close(&mut ws, 1008, error).await;
            return;
        }
        let room_name = hello["room"].as_str().unwrap().to_string();
        let build = hello["build"].as_str().unwrap().to_string();
        let map = hello["map"].as_str().unwrap().to_string();
        let id = hex::encode(rand::random::<[u8; 8]>());
        let (events, mut receiver) = mpsc::channel(32);
        let peer = Peer {
            events,
            latest: Default::default(),
            wake: Arc::new(Notify::new()),
            stop: Default::default(),
        };
        // Validation and insertion share one lock: concurrent joins cannot exceed capacity.
        let decision = {
            let mut rooms = self.rooms.lock().unwrap();
            if rooms
                .get(&room_name)
                .is_some_and(|r| r.build != build || r.map != map)
            {
                Err("Room build/map mismatch")
            } else if rooms.get(&room_name).is_some_and(|r| r.peers.len() >= 2) {
                Err("Room full")
            } else if checked {
                Ok(rooms.get(&room_name).map_or(0, |r| r.peers.len()))
            } else {
                let room = rooms.entry(room_name.clone()).or_insert(Room {
                    build,
                    map,
                    peers: HashMap::new(),
                });
                let ids: Vec<_> = room.peers.keys().cloned().collect();
                peer.event(json!({"type":"welcome","protocol":1,"id":id,"peers":ids,"hz":20}));
                for p in room.peers.values() {
                    p.event(json!({"type":"joined","id":id}));
                }
                room.peers.insert(id.clone(), peer.clone());
                Ok(0)
            }
        };
        let players = match decision {
            Ok(n) => n,
            Err(e) => {
                close(&mut ws, 1008, e).await;
                return;
            }
        };
        if checked {
            crate::diagnostics::record(
                "Relay",
                format!("Room {room_name} checked: {players}/2 players"),
            );
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                ws.send(Message::Text(
                    json!({"type":"checked","players":players,"capacity":2})
                        .to_string()
                        .into(),
                )),
            )
            .await;
            close(&mut ws, 1000, "Check complete").await;
            return;
        }
        crate::diagnostics::record("Relay", format!("Player {id} joined room {room_name}"));
        let _membership = Membership {
            relay: self.clone(),
            room: room_name.clone(),
            id: id.clone(),
        };
        let (mut sink, mut source) = ws.split();
        let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
        heartbeat.tick().await;
        let mut ping_sent = None;
        let mut last_seq = None;
        let mut tokens = 30.;
        let mut last = Instant::now();
        let mut reason = (1000, "Connection closed".to_string());
        loop {
            tokio::select! {
                _=peer.stop.cancelled()=>{ reason=(1013,"Receiver too slow".into()); break; }
                _=heartbeat.tick()=>{
                    if ping_sent.is_some() { reason=(1008,"Heartbeat timeout".into()); break; }
                    if tokio::time::timeout(Duration::from_secs(2),sink.send(Message::Ping(Vec::new().into()))).await.is_err() { break; }
                    ping_sent=Some(Instant::now());
                }
                _=peer.wake.notified()=>{
                    let mut outgoing=Vec::new();
                    while let Ok(v)=receiver.try_recv() { outgoing.push(v); }
                    outgoing.extend(std::mem::take(&mut *peer.latest.lock().unwrap()).into_values());
                    let mut failed=false;
                    for v in outgoing { if !matches!(tokio::time::timeout(Duration::from_secs(2),sink.send(Message::Text(v.to_string().into()))).await,Ok(Ok(()))) { failed=true;break; } }
                    if failed { reason=(1013,"Receiver stalled".into()); break; }
                }
                msg=source.next()=>{
                    let Some(Ok(msg))=msg else { break; };
                    match msg {
                        Message::Pong(_)=>{ ping_sent=None; }
                        Message::Ping(v)=>{ if !matches!(tokio::time::timeout(Duration::from_secs(2),sink.send(Message::Pong(v))).await,Ok(Ok(()))) { break; } }
                        Message::Close(_)=>break,
                        Message::Text(s)=>{
                            let now=Instant::now(); tokens=(tokens+now.duration_since(last).as_secs_f64()*25.).min(30.); last=now;
                            if tokens<1. { reason=(1008,"Message rate exceeded".into()); break; } tokens-=1.;
                            let parsed=serde_json::from_str::<Value>(&s).map_err(anyhow::Error::from).and_then(|v|snapshot(&v));
                            let mut pose=match parsed { Ok(v)=>v,Err(e)=>{ reason=(1008,e.to_string());break; } };
                            let seq=pose["seq"].as_u64().unwrap(); if last_seq.is_some_and(|n|seq<=n) {continue;} last_seq=Some(seq);
                            pose["id"]=json!(id);
                            if let Some(room)=self.rooms.lock().unwrap().get(&room_name) { for (other,p) in &room.peers { if other!=&id { p.pose(pose.clone()); } } }
                        }
                        _=>{reason=(1008,"Unsupported message".into());break;}
                    }
                }
            }
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            sink.send(Message::Close(Some(CloseFrame {
                code: reason.0,
                reason: reason.1.into(),
            }))),
        )
        .await;
    }
}
