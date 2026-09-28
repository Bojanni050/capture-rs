//! Recente logregels in het geheugen, voor de webinterface.
//!
//! Zodra je met `--hide-console` draait is de terminal weg; dit houdt de
//! laatste regels vast zodat `/api/logs` ze alsnog kan tonen. Werkt via een
//! `MakeWriter` die elke regel naar stderr én naar deze buffer schrijft —
//! stderr blijft dus gewoon werken voor wie de terminal wél open heeft.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Hoeveel regels we bewaren: genoeg voor het opstarten plus de laatste
/// gebeurtenissen, klein genoeg om nooit serieus geheugen te kosten.
const CAPACITY: usize = 300;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogEntry {
    /// Tijdstip zoals `tracing_subscriber::fmt` het schreef (ISO 8601).
    pub ts: String,
    /// TRACE, DEBUG, INFO, WARN of ERROR — of "?" als de regel niet te
    /// herkennen was (bv. een `println!` van een subcommando).
    pub level: String,
    /// De melding zelf, zonder tijdstempel en niveau.
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Voegt één regel toe; de oudste valt eraf als de buffer vol is.
    /// Faalt nooit: loggen mag de opname nooit in de weg zitten.
    pub fn push_line(&self, line: &str) {
        let entry = parse_line(&strip_ansi(line));
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if guard.len() >= CAPACITY {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    /// Nieuwste eerst niet nodig: dit geeft oudste eerst, zoals een terminal.
    pub fn recent(&self) -> Vec<LogEntry> {
        self.inner.lock().map(|g| g.iter().cloned().collect()).unwrap_or_default()
    }
}

/// Schrijft elke volledige regel naar de buffer, en best-effort ook naar
/// stderr. Halve regels (een `write` zonder `\n`) wachten in `pending` op
/// de rest.
///
/// De volgorde is bewust: na `detach_console` (zie --hide-console) is stderr
/// ongeldig, maar de logbuffer voor `/api/logs` moet blijven werken. Een
/// mislukte stderr-schrijf mag dus nooit een regel kosten.
pub struct TeeHandle {
    buffer: LogBuffer,
    pending: Vec<u8>,
}

impl Write for TeeHandle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
            let raw: Vec<u8> = self.pending.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&raw);
            let text = text.strip_suffix('\n').unwrap_or(&text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            if !text.trim().is_empty() {
                self.buffer.push_line(text);
            }
        }
        let _ = std::io::stderr().write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TeeWriter {
    buffer: LogBuffer,
}

impl TeeWriter {
    pub fn new(buffer: LogBuffer) -> Self {
        Self { buffer }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeWriter {
    type Writer = TeeHandle;

    fn make_writer(&'a self) -> Self::Writer {
        TeeHandle {
            buffer: self.buffer.clone(),
            pending: Vec::new(),
        }
    }
}

/// Splitst een `fmt`-regel (`2026-09-28T18:58:55.576285Z  INFO melding …`)
/// in tijdstip, niveau en melding. Alles wat er niet op lijkt wordt alsnog
/// bewaard, met niveau "?".
fn parse_line(line: &str) -> LogEntry {
    let mut words = line.split_whitespace();
    let (Some(ts), Some(level)) = (words.next(), words.next()) else {
        return LogEntry { ts: String::new(), level: "?".into(), text: line.to_string() };
    };
    if !matches!(level, "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR") {
        return LogEntry { ts: String::new(), level: "?".into(), text: line.to_string() };
    }
    let text = line
        .find(level)
        .map(|i| line[i + level.len()..].trim().to_string())
        .unwrap_or_default();
    LogEntry { ts: ts.to_string(), level: level.to_string(), text }
}

/// Haalt `\x1b[…m`-kleurcodes weg: in een terminal kleurt `fmt` de niveaus,
/// maar in de buffer en de webinterface zijn dat rommeltekens.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opstartregel_wordt_uit_elkaar_gehaald() {
        let e = parse_line("2026-09-28T18:58:55.576285Z  INFO webinterface op http://127.0.0.1:7331");
        assert_eq!(e.ts, "2026-09-28T18:58:55.576285Z");
        assert_eq!(e.level, "INFO");
        assert_eq!(e.text, "webinterface op http://127.0.0.1:7331");
    }

    #[test]
    fn velden_blijven_in_de_melding_staan() {
        let e = parse_line("2026-09-28T18:58:55.773047Z  INFO opname gestart schermen=G27Q2");
        assert_eq!(e.level, "INFO");
        assert!(e.text.contains("schermen=G27Q2"), "{e:?}");
    }

    #[test]
    fn onbekende_regel_wordt_toch_bewaard() {
        let e = parse_line("Webinterface draait op http://127.0.0.1:7331");
        assert_eq!(e.level, "?");
        assert!(!e.text.is_empty());
    }

    #[test]
    fn kleurcodes_verdwijnen_uit_de_buffer() {
        let buf = LogBuffer::new();
        buf.push_line("\x1b[32m2026-09-28T18:58:55Z  INFO hoi\x1b[0m");
        let recent = buf.recent();
        assert_eq!(recent.len(), 1);
        assert!(!recent[0].ts.contains('\x1b'), "{recent:?}");
        assert_eq!(recent[0].level, "INFO");
    }

    #[test]
    fn halve_schrijfbeurten_wachten_op_de_newline() {
        use std::io::Write;
        let buf = LogBuffer::new();
        let mut handle = TeeHandle { buffer: buf.clone(), pending: Vec::new() };
        handle.write_all(b"2026-09-28T18:58:55Z  INFO halve").unwrap();
        assert!(buf.recent().is_empty(), "zonder newline nog niets bewaren");
        handle.write_all(b" regel\n").unwrap();
        let recent = buf.recent();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].text, "halve regel");
    }

    #[test]
    fn volle_buffer_gooit_de_oudste_weg() {
        let buf = LogBuffer::new();
        for i in 0..(CAPACITY + 10) {
            buf.push_line(&format!("2026-09-28T18:58:55Z  INFO regel {i}"));
        }
        let recent = buf.recent();
        assert_eq!(recent.len(), CAPACITY);
        assert!(recent[0].text.ends_with("regel 10"), "{:?}", recent[0].text);
    }
}
