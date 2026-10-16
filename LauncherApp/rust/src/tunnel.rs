//! An explicitly requested, temporary Cloudflare Quick Tunnel. No service or trust-store changes.
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub struct Tunnel {
    pub origin: String,
    cancel: CancellationToken,
    task: JoinHandle<()>,
}
impl Tunnel {
    pub fn finished(&self) -> bool {
        self.task.is_finished()
    }
    pub async fn stop(self) {
        self.cancel.cancel();
        // Drop also cancels; borrow the handle because Tunnel implements Drop.
        let mut this = self;
        let _ = (&mut this.task).await;
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn origin_in_line(line: &str) -> Option<String> {
    line.split_whitespace().find_map(|word| {
        let origin = crate::config::origin(word, true).ok()?;
        let url = url::Url::parse(&origin).ok()?;
        let host = url.host_str()?;
        let label = host.strip_suffix(".trycloudflare.com")?;
        (!label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && url.port().is_none())
        .then_some(origin)
    })
}

pub async fn start(port: u16) -> Result<Tunnel> {
    let executable = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent().map(|p| {
                p.join(if cfg!(windows) {
                    "cloudflared.exe"
                } else {
                    "cloudflared"
                })
            })
        })
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("cloudflared"));
    let mut command = Command::new(executable);
    command.args([
        "tunnel",
        "--no-autoupdate",
        "--url",
        &format!("http://127.0.0.1:{port}"),
    ]);
    start_command(command).await
}

async fn start_command(mut command: Command) -> Result<Tunnel> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let mut child = command.spawn().context(
        "Install cloudflared first, or place it beside the launcher (see Cloudflare setup)",
    )?;
    let mut lines = BufReader::new(
        child
            .stderr
            .take()
            .context("cloudflared output unavailable")?,
    )
    .lines();
    let ready = tokio::time::timeout(Duration::from_secs(45), async {
        while let Some(line) = lines.next_line().await? {
            crate::diagnostics::record("Cloudflare", &line);
            if let Some(origin) = origin_in_line(&line) { return Ok(origin); }
        }
        bail!("cloudflared exited before returning a URL; check Console output and any existing .cloudflared configuration")
    }).await;
    let origin = match ready {
        Ok(Ok(origin)) => origin,
        other => {
            let _ = child.kill().await;
            return match other {
                Ok(Err(error)) => Err(error),
                _ => Err(anyhow::anyhow!(
                    "Cloudflare URL timed out after 45 s; check Console output, Internet access and cloudflared configuration"
                )),
            };
        }
    };
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = stop.cancelled() => { let _ = child.kill().await; break; }
                line = lines.next_line() => match line {
                    Ok(Some(line)) => crate::diagnostics::record("Cloudflare", line),
                    _ => { let _ = child.kill().await; break; }
                },
                result = child.wait() => {
                    crate::diagnostics::record("Cloudflare", format!("Tunnel exited: {result:?}"));
                    break;
                }
            }
        }
    });
    Ok(Tunnel {
        origin,
        cancel,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    fn fake_tunnel(script: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        command
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn owns_child_until_stop_or_drop_and_reports_early_exit() {
        let script = "[Console]::Error.WriteLine('https://fixture.trycloudflare.com'); Start-Sleep -Seconds 30";
        let tunnel = start_command(fake_tunnel(script)).await.unwrap();
        assert_eq!(tunnel.origin, "https://fixture.trycloudflare.com");
        assert!(!tunnel.finished());
        tokio::time::timeout(Duration::from_secs(5), tunnel.stop())
            .await
            .unwrap();
        let tunnel = start_command(fake_tunnel(script)).await.unwrap();
        let task = tunnel.task.abort_handle();
        drop(tunnel);
        tokio::time::timeout(Duration::from_secs(5), async {
            while !task.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            start_command(fake_tunnel(
                "[Console]::Error.WriteLine('fixture failure'); exit 1"
            ))
            .await
            .is_err()
        );
        let tunnel = start_command(fake_tunnel(
            "[Console]::Error.WriteLine('https://fixture.trycloudflare.com'); exit 1",
        ))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !tunnel.finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[test]
    fn extracts_only_exact_quick_tunnel_origins() {
        assert_eq!(
            origin_in_line("INF | https://some-random-name.trycloudflare.com |"),
            Some("https://some-random-name.trycloudflare.com".into())
        );
        for line in [
            "http://test.trycloudflare.com",
            "https://trycloudflare.com",
            "https://a.trycloudflare.com.evil.test",
            "https://a.b.trycloudflare.com",
            "https://a.trycloudflare.com/path",
            "https://user@a.trycloudflare.com",
            "https://a.trycloudflare.com:8000",
        ] {
            assert_eq!(origin_in_line(line), None, "{line}");
        }
    }
}
