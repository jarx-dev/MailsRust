//! A very small IMAP connection that reads the server's answers as plain text lines.
//! Used for Gmail-only requests (labels) and for saving changes, which the mail library
//! can't read: its reader gets confused by Gmail's label answers.

use crate::model::Account;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

type Tls = native_tls::TlsStream<TcpStream>;

pub struct RawImap {
    stream: BufReader<Tls>,
    next: u32,
}

/// Reads one answer line, including any {n}-sized block of raw text inside it.
fn read_line<R: BufRead>(r: &mut R) -> Result<String, String> {
    let mut out: Vec<u8> = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = r.read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("serwer zamknął połączenie".into());
        }
        let trimmed = String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']).to_string();
        out.extend_from_slice(&line);
        // "{123}" at the end: 123 bytes of raw text follow, then the line goes on.
        if let Some(open) = trimmed.rfind('{') {
            if trimmed.ends_with('}') {
                if let Ok(len) = trimmed[open + 1..trimmed.len() - 1].trim_end_matches('+').parse::<usize>() {
                    let mut buf = vec![0u8; len];
                    r.read_exact(&mut buf).map_err(|e| e.to_string())?;
                    // keep the block on the same logical line, quoted, so callers can parse it
                    while out.ends_with(b"\n") || out.ends_with(b"\r") {
                        out.pop();
                    }
                    let open_in_out = out.iter().rposition(|b| *b == b'{').unwrap_or(out.len());
                    out.truncate(open_in_out);
                    out.push(b'"');
                    for b in buf {
                        if b == b'"' || b == b'\\' {
                            out.push(b'\\');
                        }
                        out.push(b);
                    }
                    out.push(b'"');
                    continue;
                }
            }
        }
        return Ok(String::from_utf8_lossy(&out).trim_end_matches(['\r', '\n']).to_string());
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Splits a recorded server answer into lines the same way the live connection does (for tests).
pub fn split_answer(data: &[u8]) -> Vec<String> {
    let mut r = BufReader::new(std::io::Cursor::new(data.to_vec()));
    let mut out = Vec::new();
    while let Ok(l) = read_line(&mut r) {
        out.push(l);
    }
    out
}

impl RawImap {
    /// Connects and signs in the same way as the rest of the app (password or Google sign-in).
    pub fn open(acc: &Account, oauth_token: Option<String>) -> Result<Self, String> {
        let host = acc.imap_host.trim();
        let tcp = crate::net::fast_tcp(host, acc.imap_port)?;
        tcp.set_read_timeout(Some(Duration::from_secs(60))).ok();
        let connector = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let tls = if acc.imap_port == 993 {
            connector.connect(host, tcp).map_err(|e| e.to_string())?
        } else {
            // plain start, then switch to encryption
            let mut plain = BufReader::new(tcp);
            read_line(&mut plain)?;
            plain.get_mut().write_all(b"s0 STARTTLS\r\n").map_err(|e| e.to_string())?;
            loop {
                let l = read_line(&mut plain)?;
                if l.starts_with("s0 ") {
                    if !l.starts_with("s0 OK") {
                        return Err(format!("STARTTLS: {l}"));
                    }
                    break;
                }
            }
            connector.connect(host, plain.into_inner()).map_err(|e| e.to_string())?
        };
        let mut me = Self { stream: BufReader::new(tls), next: 1 };
        if acc.imap_port == 993 {
            read_line(&mut me.stream)?; // greeting
        }
        match oauth_token {
            Some(token) => {
                use base64::Engine;
                let sasl = format!("user={}\x01auth=Bearer {}\x01\x01", acc.username.trim(), token);
                let b64 = base64::engine::general_purpose::STANDARD.encode(sasl);
                me.cmd(&format!("AUTHENTICATE XOAUTH2 {b64}")).inspect_err(|_| crate::oauth::forget_tokens())?;
            }
            None => {
                me.cmd(&format!("LOGIN {} {}", quote(acc.username.trim()), quote(&acc.password)))?;
            }
        }
        Ok(me)
    }

    /// Sends one command; returns the server's untagged answer lines, or the error it gave.
    pub fn cmd(&mut self, command: &str) -> Result<Vec<String>, String> {
        let tag = format!("r{}", self.next);
        self.next += 1;
        self.stream
            .get_mut()
            .write_all(format!("{tag} {command}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        loop {
            let l = read_line(&mut self.stream)?;
            if let Some(rest) = l.strip_prefix(&format!("{tag} ")) {
                return if rest.starts_with("OK") { Ok(lines) } else { Err(rest.to_string()) };
            }
            if l.starts_with('+') {
                // the server wants more (e.g. a failed sign-in explanation); answer empty
                let _ = self.stream.get_mut().write_all(b"\r\n");
                continue;
            }
            lines.push(l);
        }
    }

    pub fn logout(mut self) {
        let _ = self.cmd("LOGOUT");
    }

    /// Folder with a special use such as \Trash or \Junk (its name as the server writes it).
    pub fn special_folder(&mut self, attr: &str, fallbacks: &[&str]) -> Option<String> {
        let lines = self.cmd("LIST \"\" \"*\"").ok()?;
        let mut names = Vec::new();
        for l in lines {
            let Some(rest) = l.strip_prefix("* LIST (") else { continue };
            let Some(close) = rest.find(')') else { continue };
            let attrs = &rest[..close];
            // after the attributes: the separator, then the name (quoted or not)
            let after = rest[close + 1..].trim_start();
            let after = if after.starts_with('"') {
                after[1..].splitn(2, '"').nth(1).unwrap_or("").trim_start()
            } else {
                after.splitn(2, ' ').nth(1).unwrap_or("").trim_start()
            };
            let name = if let Some(q) = after.strip_prefix('"') {
                q.trim_end_matches('"').replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                after.to_string()
            };
            if attrs.split_whitespace().any(|a| a.eq_ignore_ascii_case(attr)) {
                return Some(name);
            }
            names.push(name);
        }
        fallbacks
            .iter()
            .find_map(|f| names.iter().find(|n| n.eq_ignore_ascii_case(f)).cloned())
    }

    pub fn quote(s: &str) -> String {
        quote(s)
    }
}
