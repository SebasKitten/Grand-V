use eframe::egui;
use gta_browser_launcher::{
    config::{self, Settings},
    discovery::{self, Found},
    server::{self, Running},
    tls, tunnel,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

struct ConsoleWindow {
    open: AtomicBool,
    follow: AtomicBool,
    screenshot_requested: AtomicBool,
}

enum Event {
    Started(anyhow::Result<(Running, Option<tunnel::Tunnel>)>, bool),
    Stopped,
    Found(anyhow::Result<Vec<Found>>),
    Certificate(anyhow::Result<(String, String)>),
}
// Set to the published magnet URI or .torrent URL when the asset torrent is live.
const ASSET_TORRENT_URL: Option<&str> = None;

pub struct Launcher {
    runtime: Option<tokio::runtime::Runtime>,
    tx: mpsc::Sender<Event>,
    rx: mpsc::Receiver<Event>,
    folder: String,
    settings: Settings,
    code: String,
    host: Option<Running>,
    tunnel: Option<tunnel::Tunnel>,
    host_addresses: Vec<std::net::Ipv4Addr>,
    busy: bool,
    status: String,
    join: String,
    found: Vec<Found>,
    scanning: bool,
    help: bool,
    help_group: usize,
    donate: bool,
    donation_coin: Option<String>,
    share: bool,
    updates: bool,
    obtain_assets: bool,
    help_data: serde_json::Value,
    wallets: serde_json::Value,
    smoke: bool,
    created: Instant,
    auto_open: bool,
    screenshot: Option<PathBuf>,
    screenshot_requested: bool,
    console: Arc<ConsoleWindow>,
    console_status: String,
}
impl Launcher {
    pub fn new(
        folder: PathBuf,
        port: Option<u16>,
        smoke: bool,
        no_open: bool,
        screenshot: Option<PathBuf>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            runtime: Some(tokio::runtime::Runtime::new().expect("Create runtime")),
            tx,
            rx,
            folder: folder.to_string_lossy().into(),
            settings: Settings::default(),
            code: String::new(),
            host: None,
            tunnel: None,
            host_addresses: Vec::new(),
            busy: false,
            status: "Stopped".into(),
            join: String::new(),
            found: Vec::new(),
            scanning: false,
            help: false,
            help_group: 0,
            donate: false,
            donation_coin: smoke.then(|| "solana".into()),
            share: false,
            updates: false,
            obtain_assets: false,
            help_data: serde_json::from_str(include_str!("../../troubleshooting.json"))
                .expect("Bundled troubleshooting JSON"),
            wallets: serde_json::from_str(include_str!("../../donations/wallets.json"))
                .expect("Bundled wallets JSON"),
            smoke,
            created: Instant::now(),
            auto_open: !no_open && !smoke,
            screenshot,
            screenshot_requested: false,
            console: Arc::new(ConsoleWindow {
                open: AtomicBool::new(false),
                follow: AtomicBool::new(true),
                screenshot_requested: AtomicBool::new(false),
            }),
            console_status: String::new(),
        };
        if let Some(wallets) = app.wallets.as_array_mut() {
            wallets.sort_by_key(|wallet| wallet["id"].as_str() != Some("solana"));
        }
        app.load();
        if let Some(port) = port {
            app.settings.port = port;
        }
        if smoke
            && !(app.settings.cloudflare_tunnel && app.settings.mode == "external")
            && config::resolve_folder(&PathBuf::from(&app.folder)).is_ok()
        {
            app.donate = false;
            app.start(false);
        }
        app
    }
    fn load(&mut self) {
        if let Ok(folder) = config::resolve_folder(&PathBuf::from(&self.folder)) {
            self.folder = folder.to_string_lossy().into();
            match Settings::load(&folder) {
                Ok(s) => self.settings = s,
                Err(e) => self.status = format!("Could not read settings: {e}"),
            }
            match config::load_code(&folder) {
                Ok(s) => self.code = s,
                Err(e) => self.status = format!("Could not read access code: {e}"),
            }
        }
    }
    fn start(&mut self, open_game: bool) {
        if self.busy || self.host.is_some() {
            return;
        }
        self.busy = true;
        self.status = "Starting server…".into();
        let tx = self.tx.clone();
        let folder = PathBuf::from(&self.folder);
        let mut settings = self.settings.clone();
        let quick_tunnel = settings.cloudflare_tunnel && settings.mode == "external";
        let code = self.code.clone();
        self.runtime.as_ref().unwrap().spawn(async move {
            let result = async {
                let previous = settings.clone();
                if quick_tunnel {
                    settings.https = false;
                    settings.public_origin = "https://pending.invalid".into();
                }
                // Own the listener before publishing a tunnel, so a busy port cannot expose another app.
                let mut host = server::start(&folder, settings, &code).await?;
                let tunnel = if quick_tunnel {
                    match tunnel::start(host.port()).await {
                        Ok(tunnel) => {
                            if let Err(error) = host.set_public_origin(&tunnel.origin) {
                                tunnel.stop().await;
                                host.stop().await;
                                let _ = previous.save(&config::resolve_folder(&folder)?);
                                return Err(error);
                            }
                            Some(tunnel)
                        }
                        Err(error) => {
                            host.stop().await;
                            let _ = previous.save(&config::resolve_folder(&folder)?);
                            return Err(error);
                        }
                    }
                } else {
                    None
                };
                Ok((host, tunnel))
            }
            .await;
            let _ = tx.send(Event::Started(result, open_game));
        });
    }
    fn stop(&mut self) {
        if let Some(host) = self.host.take() {
            let tunnel = self.tunnel.take();
            self.busy = true;
            self.status = "Stopping server…".into();
            let tx = self.tx.clone();
            self.runtime.as_ref().unwrap().spawn(async move {
                if let Some(tunnel) = tunnel {
                    tunnel.stop().await;
                }
                host.stop().await;
                let _ = tx.send(Event::Stopped);
            });
        }
    }
    fn events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                Event::Started(result, open_game) => {
                    self.busy = false;
                    match result {
                        Ok((host, tunnel)) => {
                            self.tunnel = tunnel;
                            self.settings = host.settings.clone();
                            self.host_addresses = config::addresses();
                            self.code = host.code.clone();
                            self.status =
                                format!("Running: {}", host.url().split('#').next().unwrap_or(""));
                            if let Some(error) = &host.discovery_error {
                                self.status.push_str(&format!(
                                    " • Discovery unavailable: {error}; share the link manually"
                                ));
                            }
                            if open_game && self.auto_open {
                                let _ = webbrowser::open(&host.url());
                            }
                            self.host = Some(host);
                        }
                        Err(e) => self.status = format!("Could not start: {e:#}"),
                    }
                }
                Event::Stopped => {
                    self.busy = false;
                    self.status = if self.status.starts_with("Cloudflare tunnel exited") {
                        "Stopped: Cloudflare tunnel exited. Check Console output; start again for a new link.".into()
                    } else {
                        "Stopped".into()
                    };
                    self.host_addresses.clear();
                    self.share = false;
                }
                Event::Found(result) => {
                    self.scanning = false;
                    match result {
                        Ok(rows) => {
                            self.status = format!("Found {} LAN server(s)", rows.len());
                            self.found = rows;
                        }
                        Err(e) => self.status = format!("Discovery failed: {e}"),
                    }
                }
                Event::Certificate(result) => {
                    self.busy = false;
                    match result {
                        Ok((cert, key)) => {
                            self.settings.tls_cert = cert;
                            self.settings.tls_key = key;
                            self.settings.https = true;
                            self.status =
                                "LAN certificate created; share only lan-root-ca.cer".into();
                        }
                        Err(e) => self.status = format!("Certificate generation failed: {e:#}"),
                    }
                }
            }
        }
        if let Some(host) = &self.host {
            self.settings.port = host.port();
        }
        if self.tunnel.as_ref().is_some_and(|t| t.finished()) {
            self.tunnel.take();
            self.stop();
            self.status = "Cloudflare tunnel exited; stopping server. Check Console output, then start again for a new link.".into();
        }
    }
    fn qr(ui: &mut egui::Ui, value: &str) {
        if let Ok(code) = qrcode::QrCode::new(value) {
            let width = code.width();
            let side = 250.;
            let cell = side / (width + 8) as f32;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
            ui.painter().rect_filled(rect, 0., egui::Color32::WHITE);
            for y in 0..width {
                for x in 0..width {
                    if code[(x, y)] == qrcode::Color::Dark {
                        let p = rect.min + egui::vec2((x + 4) as f32 * cell, (y + 4) as f32 * cell);
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(p, egui::vec2(cell, cell)),
                            0.,
                            egui::Color32::BLACK,
                        );
                    }
                }
            }
        }
    }
    fn help_sections(ui: &mut egui::Ui, value: &serde_json::Value) {
        if let Some(rows) = value.as_array() {
            for (index, row) in rows.iter().enumerate() {
                let title = row["title"].as_str().unwrap_or("Help");
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(26, 34, 45))
                    .stroke(egui::Stroke::new(
                        1_f32,
                        egui::Color32::from_rgb(75, 96, 118),
                    ))
                    .corner_radius(7)
                    .inner_margin(14)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if let Some(url) = row["url"].as_str() {
                            ui.hyperlink_to(egui::RichText::new(title).size(17.), url);
                        } else {
                            egui::CollapsingHeader::new(
                                egui::RichText::new(title)
                                    .size(18.)
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .id_salt(title)
                            .default_open(index == 0)
                            .show(ui, |ui| {
                                ui.add_space(12.);
                                if let Some(steps) = row["steps"].as_array() {
                                    for (number, step) in steps.iter().enumerate() {
                                        if let Some(body) = step.as_str() {
                                            ui.horizontal_top(|ui| {
                                                ui.label(
                                                    egui::RichText::new(format!("{}.", number + 1))
                                                        .size(16.)
                                                        .strong()
                                                        .color(egui::Color32::from_rgb(
                                                            145, 205, 255,
                                                        )),
                                                );
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(body)
                                                            .size(16.)
                                                            .line_height(Some(24.))
                                                            .color(egui::Color32::from_rgb(
                                                                240, 244, 250,
                                                            )),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                            ui.add_space(12.);
                                        }
                                    }
                                }
                            });
                        }
                    });
                ui.add_space(10.);
            }
        }
    }
    fn layout(&mut self, ctx: &egui::Context) {
        self.events();
        if self.console_status != self.status {
            gta_browser_launcher::diagnostics::record("Launcher", &self.status);
            self.console_status = self.status.clone();
        }
        egui::CentralPanel::default().show(ctx,|ui| {egui::ScrollArea::vertical().show(ui,|ui| {
            egui::Frame::new().fill(egui::Color32::from_rgb(22, 48, 57)).stroke(egui::Stroke::new(1_f32, egui::Color32::from_rgb(50, 122, 130))).corner_radius(8).inner_margin(16).show(ui, |ui| {ui.set_min_width(ui.available_width());ui.heading(egui::RichText::new(gta_browser_launcher::APP_NAME).color(egui::Color32::from_rgb(154, 239, 214)));ui.label("Play here, host for other players, or join an existing server.");});ui.add_space(8.);
            ui.collapsing("Join a server — no game files needed",|ui| {
                ui.horizontal(|ui|{ui.label("Game website");ui.text_edit_singleline(&mut self.join);if ui.button("Open game").clicked(){match config::join_url(&self.join){Ok(url)=>{if let Err(e)=webbrowser::open(&url){self.status=e.to_string();}},Err(e)=>self.status=e.to_string()}}});
                if ui.add_enabled(!self.scanning,egui::Button::new("Find LAN servers")).clicked(){self.scanning=true;let tx=self.tx.clone();self.runtime.as_ref().unwrap().spawn(async move {let _=tx.send(Event::Found(discovery::discover().await));});}
                for found in &self.found {ui.horizontal(|ui|{ui.label(&found.name);if ui.link(&found.url).clicked(){let _=webbrowser::open(&found.url);}if ui.small_button("Copy link").clicked(){ctx.copy_text(found.url.clone());}});if !found.fingerprint.is_empty(){ui.small(format!("CA SHA-256: {}",found.fingerprint));}}
            });
            ui.separator();ui.heading(egui::RichText::new("Play / host").color(egui::Color32::from_rgb(137, 204, 255)));
            let editable=!self.busy && self.host.is_none();
            ui.add_enabled_ui(editable,|ui| {
                ui.horizontal(|ui|{ui.label("Game folder");let changed=ui.text_edit_singleline(&mut self.folder).lost_focus();if ui.button("Browse…").clicked(){if let Some(path)=rfd::FileDialog::new().set_title("Select playgta5-offline").pick_folder(){self.folder=path.to_string_lossy().into();self.load();}}else if changed {self.load();}});
                egui::ComboBox::from_label("Audience").selected_text(match self.settings.mode.as_str(){"lan"=>"LAN / school network","external"=>"Public website",_=>"This PC"}).show_ui(ui,|ui|{
                    ui.selectable_value(&mut self.settings.mode,"device".into(),"This PC");ui.selectable_value(&mut self.settings.mode,"lan".into(),"LAN / school network");ui.selectable_value(&mut self.settings.mode,"external".into(),"Public website");
                });
                ui.checkbox(&mut self.settings.multiplayer,"Enable multiplayer (2 players per room)");
                if self.settings.multiplayer {ui.horizontal(|ui|{ui.label("Access code");ui.text_edit_singleline(&mut self.code);if ui.button("Generate").clicked(){self.code=config::random_token();}if ui.button("Copy").clicked(){ctx.copy_text(self.code.clone());}});}
                if self.settings.mode=="external" {
                    ui.checkbox(&mut self.settings.cloudflare_tunnel,"Cloudflare free tunnel (temporary HTTPS link)");
                    if self.settings.cloudflare_tunnel {
                        ui.small("Start creates a public HTTPS link. No domain, account, port forwarding or client certificate import needed. Keep this launcher open.");
                        ui.small("Anyone with the link can access the game and save APIs. The multiplayer code protects only the relay. URL changes on restart; browser saves/cache belong to each URL.");
                        ui.small("Install cloudflared once, then place it beside the launcher or on PATH. Direct HTTPS is automatically disabled for this mode.");
                        ui.horizontal_wrapped(|ui| {
                            ui.hyperlink_to("Cloudflare setup / downloads","https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/downloads/");
                            if ui.button("Copy install command (PowerShell / CMD)").clicked(){ctx.copy_text("winget install --id Cloudflare.cloudflared --exact".into());}
                        });
                        ui.small("Windows: open PowerShell or Command Prompt, paste the copied command and press Enter. After installation, restart this launcher so it picks up PATH changes.");
                        ui.small("Then click Start server: the backend, tunnel and HTTPS link are set up automatically. Use Share game link when ready. Start & open game also opens the game on this PC.");
                        ui.small("Quick Tunnels: no uptime guarantee, 200 in-flight requests, no SSE. Asset downloads use your upload bandwidth; all proxied clients share the backend's IP limits.");
                    } else {ui.horizontal(|ui|{ui.label("Public HTTPS origin");ui.text_edit_singleline(&mut self.settings.public_origin);});ui.small("Configure the domain, HTTPS reverse proxy and firewall before sharing.");}
                }
                ui.collapsing("Advanced server settings",|ui| {
                    ui.horizontal(|ui|{ui.label("Game port");ui.add(egui::DragValue::new(&mut self.settings.port).range(1024..=65535));ui.label("Maximum clients (0 = unlimited)");ui.add(egui::DragValue::new(&mut self.settings.max_clients).range(0..=10000));});
                    let through_tunnel = self.settings.cloudflare_tunnel && self.settings.mode=="external";
                    ui.horizontal(|ui| {
                        ui.label("Maximum concurrent connections / streams (0 = unlimited)");
                        if through_tunnel {
                            let mut limit = 200_usize;
                            ui.add_enabled(false, egui::DragValue::new(&mut limit));
                        } else {
                            ui.add(egui::DragValue::new(&mut self.settings.max_connections));
                        }
                    });
                    ui.small(if through_tunnel {"Cloudflare mode uses 200 connections. Your direct-hosting limit is retained; Quick Tunnel also has its own 200 in-flight request limit."} else {"Applies to direct HTTP/HTTPS and manual proxy hosting. No fixed application cap; 0 removes the connection limit. Counts open connections, including idle connections and multiplayer relay sessions, rather than players."});
                    ui.horizontal(|ui|{ui.label(if through_tunnel {"MiB/s shared by tunnel users (0 = unlimited)"} else {"MiB/s per IP (0 = unlimited)"});ui.add(egui::DragValue::new(&mut self.settings.speed_mib).range(0.0..=100000.0).speed(0.1));});
                    ui.small("Suggested starting point: 10 MiB/s (~84 Mbit/s) per direct client if bandwidth allows. On a fast LAN, 0 (unlimited) avoids artificial asset throttling.");
                    ui.small("Internet hosting: leave about 20% upload headroom. Per-player budget = measured host upload (Mbit/s) x 0.8 / 8.39 / active players. Example: 100 Mbit/s upload with 2 players gives about 4.8 MiB/s each.");
                    if through_tunnel {
                        ui.small("Cloudflare: this field caps ALL tunnel users together. With 100 Mbit/s upload, about 9.5 MiB/s TOTAL leaves 20% headroom; two equally active players share about 4.8 MiB/s each.");
                    } else {
                        ui.small("Limits are per source IP, not per browser. Players behind the same proxy/IP share the cap; a reverse proxy may combine all players.");
                    }
                    ui.small("These are tuning guidelines, not a measured game minimum. Uncached assets need more bandwidth. If assets load late or stall, raise the cap where bandwidth allows or host fewer players. A higher cap cannot increase your Internet upload speed.");
                    ui.add_enabled(!(self.settings.cloudflare_tunnel && self.settings.mode=="external"),egui::Checkbox::new(&mut self.settings.https,"Use direct HTTPS / custom certificates"));
                    for (label,value) in [("Certificate PEM",&mut self.settings.tls_cert),("Private key PEM",&mut self.settings.tls_key)] {ui.horizontal(|ui|{ui.label(label);ui.text_edit_singleline(value);if ui.small_button("Browse…").clicked(){if let Some(path)=rfd::FileDialog::new().add_filter("PEM / key",&["pem","key"]).pick_file(){*value=path.to_string_lossy().into();}}});}
                    if ui.button("Create / renew LAN certificate").clicked(){match config::resolve_folder(&PathBuf::from(&self.folder)){Ok(folder)=>{self.busy=true;let tx=self.tx.clone();self.runtime.as_ref().unwrap().spawn(async move {let result=tokio::task::spawn_blocking(move||tls::generate(&folder)).await.map_err(anyhow::Error::from).and_then(|v|v);let _=tx.send(Event::Certificate(result));});},Err(e)=>self.status=e.to_string()}}
                    ui.small("LAN creates a private CA automatically. Trust is installed manually on each client.");
                });
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(editable,egui::Button::new("Start server").fill(egui::Color32::from_rgb(28, 99, 80))).clicked(){self.start(false);}
                if ui.add_enabled(editable,egui::Button::new("Start & open game").fill(egui::Color32::from_rgb(32, 89, 139))).clicked(){self.start(true);}
                if ui.add_enabled(self.host.is_some() && !self.busy,egui::Button::new("Stop server").fill(egui::Color32::from_rgb(119, 48, 59))).clicked(){self.stop();}
                if ui.add(egui::Button::new("Console output").fill(egui::Color32::from_rgb(71, 57, 111))).clicked(){if self.console.open.load(Ordering::Relaxed) {ctx.send_viewport_cmd_to(egui::ViewportId::from_hash_of("grandv-console"),egui::ViewportCommand::Focus);}self.console.open.store(true,Ordering::Relaxed);}
                if let Some(host)=&self.host {if ui.button("Open game").clicked(){let _=webbrowser::open(&host.url());}if host.settings.mode!="device" && ui.button("Share game link").clicked(){self.share=true;}}
            });
            let status_color=if self.busy {egui::Color32::from_rgb(245, 203, 116)} else if self.host.is_some() {egui::Color32::from_rgb(147, 232, 184)} else if self.status=="Stopped" {egui::Color32::from_rgb(176, 194, 215)} else {egui::Color32::from_rgb(245, 203, 116)};egui::Frame::new().fill(egui::Color32::from_rgb(24, 36, 51)).corner_radius(6).inner_margin(10).show(ui,|ui|{ui.label(egui::RichText::new(&self.status).color(status_color));});
            if let Some(host)=&self.host {
                let scheme=host.settings.scheme();let port=host.port();
                ui.label(format!("Localhost: {scheme}://localhost:{port}/launcher.html"));
                ui.label(format!("Loopback IP: {scheme}://127.0.0.1:{port}/launcher.html"));
                if host.settings.bind_ip().is_unspecified() {
                    if self.host_addresses.is_empty() {ui.label("Local network: no IPv4 address found.");}
                    for ip in &self.host_addresses {ui.label(format!("Local network: {scheme}://{ip}:{port}/launcher.html"));}
                } else if host.settings.mode=="device" {
                    ui.label("Local network access is off. Select LAN / school network before starting to allow other devices.");
                } else {ui.label("Backend listens on localhost; network clients connect through the configured public website.");}
                if !host.settings.public_origin.is_empty() {ui.label(format!("Public website: {}/launcher.html",host.settings.public_origin.trim_end_matches('/')));}
            }
            if let Some(host)=&self.host {ui.small(format!("Listener port: {} • Multiplayer: {}",host.port(),if host.settings.multiplayer{"enabled"}else{"disabled"}));if host.settings.multiplayer && ui.button("Copy access code").clicked(){ctx.copy_text(host.code.clone());}}
            ui.separator();ui.horizontal_wrapped(|ui|{if ui.button("Need to obtain assets?").clicked(){self.obtain_assets=true;}if ui.button("Troubleshooting").clicked(){self.help=true;}if ui.button("Optional donation").clicked(){if !self.donate {self.donation_coin=self.wallets.as_array().and_then(|wallets|wallets.first()).and_then(|wallet|wallet["id"].as_str()).map(str::to_owned);}self.donate=true;}if ui.button("Updates and info").clicked(){self.updates=true;}});
        });});
        if self.obtain_assets {
            egui::Window::new("Need to obtain assets?")
                .open(&mut self.obtain_assets)
                .default_width(420.)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Game assets are downloaded separately from this launcher.");
                    if let Some(url) = ASSET_TORRENT_URL {
                        if ui.button("Open asset torrent").clicked() {
                            if let Err(error) = webbrowser::open(url) {
                                self.status = format!("Could not open asset torrent: {error}");
                            }
                        }
                    } else {
                        ui.label("The asset torrent is not live yet. The download link is coming soon.");
                        ui.add_enabled(false, egui::Button::new("Torrent — coming soon"));
                    }
                    ui.separator();
                    ui.label("Once downloaded and extracted, use Browse to select the game folder containing mirror.");
                });
        }
        if self.updates {
            let mut open = self.updates;
            egui::Window::new("Updates and info")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_pos(ctx.content_rect().center() - egui::vec2(194., 45.))
                .movable(true)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        for (label, url) in [
                            ("Telegram", "https://t.me/reGRANDTA"),
                            ("Discord", "https://discord.gg/Td8Jdjw3Wf"),
                            ("GitHub", "https://github.com/SebasKitten/Grand-V"),
                        ] {
                            if ui
                                .add_sized([120., 36.], egui::Button::new(label))
                                .clicked()
                            {
                                if let Err(error) = webbrowser::open(url) {
                                    self.status = format!("Could not open {label}: {error}");
                                }
                            }
                        }
                    });
                });
            self.updates = open;
        }
        if self.share {
            let mut open = self.share;
            egui::Window::new("Share game link").open(&mut open).show(ctx,|ui| {if let Some(host)=&self.host {
            let url=host.share_url();ui.label(&url);Self::qr(ui,&url);if ui.button("Copy link").clicked(){ctx.copy_text(url.clone());}
            if ui.button("Copy player instructions").clicked(){ctx.copy_text(format!("Open {url} in Chrome or Edge. Choose Multiplayer, enter the host's access code, then Play."));}
            if !host.fingerprint().is_empty(){ui.label(format!("CA SHA-256: {}",host.fingerprint()));if ui.button("Open public client certificate").clicked(){let cert=PathBuf::from(&host.settings.tls_cert).with_file_name("lan-root-ca.cer");let _=open_file(&cert);}ui.small("Share only the .cer file. Keep .key files private; client trust is a manual step.");}
        }});
            self.share = open;
        }
        if self.help {
            let mut open = self.help;
            egui::Window::new("Troubleshooting")
                .open(&mut open)
                .default_size([760., 640.])
                .show(ctx, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (index, title) in
                            ["Launcher & connections", "Browser & game", "Official help"]
                                .iter()
                                .enumerate()
                        {
                            ui.selectable_value(&mut self.help_group, index, *title);
                        }
                    });
                    ui.add_space(10.);
                    ui.label("Choose a topic below. Click its heading to show or hide the steps.");
                    ui.label(format!(
                        "Guide updated {}",
                        self.help_data["updated"].as_str().unwrap_or("")
                    ));
                    ui.add_space(14.);
                    let group = ["server", "web", "sources"][self.help_group];
                    egui::ScrollArea::vertical().id_salt(group).show(ui, |ui| {
                        ui.push_id(group, |ui| Self::help_sections(ui, &self.help_data[group]))
                    });
                });
            self.help = open;
        }
        if self.donate {
            let mut open = self.donate;
            egui::Window::new("Optional donation")
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Donations are optional. The launcher does not require payment.");
                    if let Some(wallets) = self.wallets.as_array() {
                        for wallet in wallets {
                            let id = wallet["id"].as_str().unwrap_or("");
                            let selected = self.donation_coin.as_deref() == Some(id);
                            let name = wallet["name"].as_str().unwrap_or("Wallet");
                            let header = ui.selectable_label(selected, format!("    {name}"));
                            let center =
                                egui::pos2(header.rect.left() + 8., header.rect.center().y);
                            let points = if selected {
                                vec![
                                    center + egui::vec2(-4., -2.),
                                    center + egui::vec2(4., -2.),
                                    center + egui::vec2(0., 3.),
                                ]
                            } else {
                                vec![
                                    center + egui::vec2(-2., -4.),
                                    center + egui::vec2(-2., 4.),
                                    center + egui::vec2(3., 0.),
                                ]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(
                                points,
                                ui.visuals().text_color(),
                                egui::Stroke::NONE,
                            ));
                            if header.clicked() {
                                self.donation_coin = if selected { None } else { Some(id.into()) };
                            }
                            if self.donation_coin.as_deref() == Some(id) {
                                ui.indent(id, |ui| {
                                    let address = wallet["address"].as_str().unwrap_or("");
                                    ui.label(format!(
                                        "Network: {}",
                                        wallet["network"].as_str().unwrap_or("")
                                    ));
                                    ui.label(address);
                                    Self::qr(ui, address);
                                    if ui.button("Copy address").clicked() {
                                        ctx.copy_text(address.into());
                                    }
                                });
                            }
                        }
                    }
                });
            self.donate = open;
        }
        if self.console.open.load(Ordering::Relaxed) {
            self.show_console(ctx);
        }
    }
    fn console_contents(ui: &mut egui::Ui, console: &ConsoleWindow) {
        let mut follow = console.follow.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut follow, "Follow output").changed() {
                console.follow.store(follow, Ordering::Relaxed);
            }
            if ui.button("Copy output").clicked() {
                ui.ctx()
                    .copy_text(gta_browser_launcher::diagnostics::snapshot());
            }
            if ui.button("Clear").clicked() {
                gta_browser_launcher::diagnostics::clear();
            }
        });
        ui.small("Closing this window hides the console; the server keeps running.");
        ui.separator();
        let text = gta_browser_launcher::diagnostics::snapshot();
        egui::ScrollArea::both()
            .stick_to_bottom(follow)
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(if text.is_empty() {
                            "Waiting for output…"
                        } else {
                            &text
                        })
                        .monospace(),
                    )
                    .selectable(true)
                    .wrap_mode(egui::TextWrapMode::Extend),
                );
            });
    }
    fn show_console(&self, ctx: &egui::Context) {
        let console = self.console.clone();
        let smoke = self.smoke;
        let screenshot = self.screenshot.clone();
        let created = self.created;
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("grandv-console"),
            egui::ViewportBuilder::default()
                .with_title("GrandV Console output")
                .with_inner_size([900., 500.])
                .with_min_inner_size([480., 240.])
                .with_icon(console_icon()),
            move |ctx, class| {
                if class == egui::ViewportClass::Embedded {
                    let mut open = console.open.load(Ordering::Relaxed);
                    egui::Window::new("Console output")
                        .open(&mut open)
                        .show(ctx, |ui| Self::console_contents(ui, &console));
                    console.open.store(open, Ordering::Relaxed);
                } else {
                    if ctx.input(|i| i.viewport().close_requested()) {
                        console.open.store(false, Ordering::Relaxed);
                        ctx.request_repaint_of(egui::ViewportId::ROOT);
                        return;
                    }
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::central_panel(&ctx.style())
                                .fill(egui::Color32::from_rgb(12, 12, 12)),
                        )
                        .show(ctx, |ui| Self::console_contents(ui, &console));
                    ctx.request_repaint_after(Duration::from_millis(100));
                    if smoke
                        && screenshot.is_some()
                        && created.elapsed() > Duration::from_secs(1)
                        && !console.screenshot_requested.swap(true, Ordering::Relaxed)
                    {
                        ctx.send_viewport_cmd(
                            egui::ViewportCommand::Screenshot(Default::default()),
                        );
                    }
                    if let Some(path) = &screenshot {
                        ctx.input(|i| {
                            for event in &i.events {
                                if let egui::Event::Screenshot { image, .. } = event {
                                    let pixels: Vec<_> =
                                        image.pixels.iter().flat_map(|c| c.to_array()).collect();
                                    let _ = image::save_buffer(
                                        path.with_extension("console.png"),
                                        &pixels,
                                        image.size[0] as u32,
                                        image.size[1] as u32,
                                        image::ColorType::Rgba8,
                                    );
                                }
                            }
                        });
                    }
                }
            },
        );
    }
}
fn console_icon() -> std::sync::Arc<egui::IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<egui::IconData>> = std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let image =
            image::load_from_memory(include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.png")))
                .expect("Embedded app icon")
                .into_rgba8();
        let image =
            image::imageops::resize(&image, 128, 128, image::imageops::FilterType::Lanczos3);
        std::sync::Arc::new(egui::IconData {
            rgba: image.into_raw(),
            width: 128,
            height: 128,
        })
    })
    .clone()
}
fn open_file(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer.exe")
            .arg(path)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(path).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(path.parent().unwrap_or(path))
            .spawn()?;
    }
    Ok(())
}
impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.layout(ctx);
        ctx.request_repaint_after(Duration::from_millis(100));
        if self.screenshot.is_some()
            && !self.screenshot_requested
            && self.created.elapsed() > Duration::from_secs(1)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.screenshot_requested = true;
        }
        if let Some(path) = &self.screenshot {
            ctx.input(|i| {
                for event in &i.events {
                    if let egui::Event::Screenshot { image, .. } = event {
                        let pixels: Vec<_> =
                            image.pixels.iter().flat_map(|c| c.to_array()).collect();
                        let _ = image::save_buffer(
                            path,
                            &pixels,
                            image.size[0] as u32,
                            image.size[1] as u32,
                            image::ColorType::Rgba8,
                        );
                    }
                }
            });
        }
        if self.smoke && self.created.elapsed() > Duration::from_secs(3) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
impl Drop for Launcher {
    fn drop(&mut self) {
        if let Some(tunnel) = self.tunnel.take() {
            if let Some(rt) = &self.runtime {
                rt.block_on(tunnel.stop());
            }
        }
        if let Some(host) = self.host.take() {
            host.request_stop();
            if let Some(rt) = &self.runtime {
                rt.block_on(host.stop());
            }
        }
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(3));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_works_without_game_folder_and_contains_embedded_data() {
        let temp = tempfile::tempdir().unwrap();
        let mut app = Launcher::new(temp.path().into(), None, false, true, None);
        assert_eq!(app.wallets.as_array().unwrap().len(), 4);
        assert!(app.help_data["web"].as_array().unwrap().len() > 3);
        app.help = true;
        app.donate = true;
        app.donation_coin = Some("solana".into());
        app.updates = true;
        let ctx = egui::Context::default();
        let output = ctx.run(egui::RawInput::default(), |ctx| app.layout(ctx));
        assert!(!output.shapes.is_empty());
        assert!(app.host.is_none());
        app.settings.mode = "external".into();
        app.settings.cloudflare_tunnel = true;
        app.help = false;
        app.donate = false;
        app.updates = false;
        let output = ctx.run(egui::RawInput::default(), |ctx| app.layout(ctx));
        assert!(!output.shapes.is_empty());
        assert!(app.host.is_none());
    }
    #[test]
    fn gui_controller_start_stop_and_drop_release_owned_host() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("mirror/playgta5.com");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("launcher.html"), "GUI fixture").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut app = Launcher::new(temp.path().into(), Some(port), false, true, None);
        app.settings.multiplayer = true;
        app.code = "GUI-code".into();
        let wait = |app: &mut Launcher| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.busy && Instant::now() < deadline {
                app.events();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!app.busy, "{}", app.status);
        };
        app.start(false);
        wait(&mut app);
        assert!(app.host.is_some(), "{}", app.status);
        assert_eq!(app.code, "GUI-code");
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
        app.stop();
        wait(&mut app);
        assert!(app.host.is_none());
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        app.start(false);
        wait(&mut app);
        assert!(app.host.is_some());
        drop(app);
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    }
}
