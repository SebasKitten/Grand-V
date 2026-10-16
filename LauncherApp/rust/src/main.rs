#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod ui;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "GrandV Back end launcher; game assets remain external")]
struct Args {
    #[arg(long)]
    folder: Option<PathBuf>,
    /// Run the host without a GUI; read its existing launcher-settings.json.
    #[arg(long)]
    headless: bool,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    no_open: bool,
    /// Open and automatically close a native GUI to check display initialization.
    #[arg(long)]
    ui_smoke: bool,
    #[arg(long)]
    ui_screenshot: Option<PathBuf>,
}
fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    #[cfg(windows)]
    if args.headless {
        // GUI launches have no console. Headless launches attach to their calling terminal for logs and Ctrl+C.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    let folder = args.folder.unwrap_or_else(|| {
        let executable = std::env::current_exe().unwrap_or_default();
        if let Some(bundle) = executable
            .ancestors()
            .find(|p| p.extension().is_some_and(|s| s == "app"))
        {
            bundle.parent().unwrap_or(bundle).to_path_buf()
        } else {
            executable.parent().unwrap_or(&executable).to_path_buf()
        }
    });
    if args.headless {
        let runtime = tokio::runtime::Runtime::new()?;
        return runtime.block_on(async {
            let folder = gta_browser_launcher::config::resolve_folder(&folder)?;
            let mut settings = gta_browser_launcher::config::Settings::load(&folder)?;
            if let Some(port) = args.port {
                settings.port = port;
            }
            let code = gta_browser_launcher::config::load_code(&folder)?;
            let host = gta_browser_launcher::server::start(&folder, settings, &code).await?;
            println!("Running: {}", host.url());
            if !args.no_open {
                let _ = webbrowser::open(&host.url());
            }
            shutdown_signal().await?;
            host.stop().await;
            Ok(())
        });
    }
    let logo = image::load_from_memory(include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.png")))?
        .into_rgba8();
    let logo = image::imageops::resize(&logo, 256, 256, image::imageops::FilterType::Lanczos3);
    let icon = eframe::egui::IconData {
        rgba: logo.into_raw(),
        width: 256,
        height: 256,
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(gta_browser_launcher::APP_NAME)
            .with_app_id("com.grandv.backendlauncher")
            .with_icon(icon)
            .with_inner_size([800., 740.])
            .with_min_inner_size([640., 500.]),
        ..Default::default()
    };
    let screenshot = args.ui_screenshot.clone();
    eframe::run_native(
        gta_browser_launcher::APP_NAME,
        options,
        Box::new(move |cc| {
            let mut style = (*cc.egui_ctx.style()).clone();
            style.text_styles.insert(
                eframe::egui::TextStyle::Heading,
                eframe::egui::FontId::proportional(24.),
            );
            for text in [
                eframe::egui::TextStyle::Body,
                eframe::egui::TextStyle::Button,
            ] {
                style
                    .text_styles
                    .insert(text, eframe::egui::FontId::proportional(16.));
            }
            style.text_styles.insert(
                eframe::egui::TextStyle::Small,
                eframe::egui::FontId::proportional(13.),
            );
            style.spacing.item_spacing = eframe::egui::vec2(8., 10.);
            style.visuals.override_text_color = Some(eframe::egui::Color32::from_gray(230));
            let rgb = eframe::egui::Color32::from_rgb;
            style.visuals.panel_fill = rgb(15, 23, 34);
            style.visuals.window_fill = rgb(22, 32, 46);
            style.visuals.extreme_bg_color = rgb(10, 17, 27);
            style.visuals.faint_bg_color = rgb(27, 40, 55);
            style.visuals.hyperlink_color = rgb(125, 204, 255);
            style.visuals.selection.bg_fill = rgb(27, 105, 129);
            style.visuals.selection.stroke = eframe::egui::Stroke::new(1_f32, rgb(171, 237, 246));
            style.visuals.widgets.inactive.bg_fill = rgb(36, 53, 73);
            style.visuals.widgets.inactive.weak_bg_fill = rgb(36, 53, 73);
            style.visuals.widgets.inactive.bg_stroke =
                eframe::egui::Stroke::new(1_f32, rgb(66, 88, 112));
            style.visuals.widgets.hovered.bg_fill = rgb(47, 78, 101);
            style.visuals.widgets.hovered.weak_bg_fill = rgb(47, 78, 101);
            style.visuals.widgets.hovered.bg_stroke =
                eframe::egui::Stroke::new(1_f32, rgb(110, 199, 222));
            style.visuals.widgets.active.bg_fill = rgb(27, 105, 129);
            style.visuals.widgets.active.weak_bg_fill = rgb(27, 105, 129);
            style.visuals.widgets.active.bg_stroke =
                eframe::egui::Stroke::new(1_f32, rgb(149, 226, 237));
            style.spacing.button_padding = eframe::egui::vec2(12., 8.);
            cc.egui_ctx.set_style(style);
            Ok(Box::new(ui::Launcher::new(
                folder,
                args.port,
                args.ui_smoke,
                args.no_open,
                args.ui_screenshot,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if let Some(path) = screenshot {
        anyhow::ensure!(path.is_file(), "Native screenshot was not saved");
    }
    Ok(())
}
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let mut interrupt = tokio::signal::windows::ctrl_break()?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result,_=interrupt.recv()=>Ok(())}
    }
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result,_=terminate.recv()=>Ok(())}
    }
}
