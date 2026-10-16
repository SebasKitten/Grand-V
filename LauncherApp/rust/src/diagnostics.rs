use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
};

const MAX_LINES: usize = 1500;
const MAX_BYTES: usize = 512 * 1024;
#[derive(Default)]
struct Buffer {
    lines: VecDeque<String>,
    bytes: usize,
}
fn buffer() -> &'static Mutex<Buffer> {
    static LOG: OnceLock<Mutex<Buffer>> = OnceLock::new();
    LOG.get_or_init(Default::default)
}
pub fn record(source: &str, message: impl AsRef<str>) {
    let now = time::OffsetDateTime::now_utc();
    let mut log = buffer().lock().unwrap_or_else(|e| e.into_inner());
    for text in message.as_ref().lines().take(1000) {
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .take(2048)
            .collect();
        let line = format!(
            "[{:02}:{:02}:{:02} UTC] [{source}] {text}\n",
            now.hour(),
            now.minute(),
            now.second()
        );
        log.bytes += line.len();
        log.lines.push_back(line);
        while log.lines.len() > MAX_LINES || log.bytes > MAX_BYTES {
            if let Some(line) = log.lines.pop_front() {
                log.bytes -= line.len();
            }
        }
    }
}
pub fn snapshot() -> String {
    buffer()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .lines
        .iter()
        .cloned()
        .collect()
}
pub fn clear() {
    *buffer().lock().unwrap_or_else(|e| e.into_inner()) = Buffer::default();
}

#[cfg(test)]
mod tests {
    #[test]
    fn output_is_bounded_even_for_large_multiline_messages() {
        super::clear();
        for _ in 0..2000 {
            super::record("Test", "x".repeat(4096));
        }
        let output = super::snapshot();
        assert!(output.len() <= super::MAX_BYTES);
        assert!(output.lines().count() <= super::MAX_LINES);
        assert!(output.contains("[Test]"));
        super::clear();
        assert!(super::snapshot().is_empty());
    }
}
