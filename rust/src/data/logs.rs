//! Claude Code activity from its local transcripts (~/.claude/projects/**/*.jsonl).
//! Reads only appended bytes per file, de-duplicates streamed repeats by
//! (message.id, requestId), and counts input + output + cache-write tokens (cache reads
//! excluded). Same rules as the Python LogScanner.

use crate::config::{Store, projects_dir};
use crate::data::{now, parse_iso};
use crate::logf;
use chrono::{Duration as CDuration, Local, TimeZone, Timelike};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const WEEKS: usize = 4; // heatmap pages: this week + 3 earlier 7-day blocks
pub const KEEP_DAYS: f64 = (WEEKS * 7 + 1) as f64; // Claude Code deletes transcripts after ~30 days
const CHUNK: usize = 4 << 20;

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<f64>,
    output_tokens: Option<f64>,
    cache_creation_input_tokens: Option<f64>,
}

#[derive(Deserialize)]
struct Msg {
    id: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct Line {
    timestamp: Option<String>,
    requestId: Option<String>,
    sessionId: Option<String>,
    message: Option<Msg>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Week {
    pub heat: Vec<[f64; 24]>, // 7 days, oldest first
    pub day_labels: Vec<String>,
    pub day_dates: Vec<String>,
    pub label: String,
    pub total: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stats {
    pub today: f64,
    pub msgs: u64,
    pub week: f64,
    pub burn: f64,
    pub active: usize,
    pub weeks: Vec<Week>, // newest first
}

#[derive(Default)]
pub struct Scanner {
    offsets: HashMap<PathBuf, u64>,
    partial: HashMap<PathBuf, Vec<u8>>,
    /// (message id, request id) -> (epoch, tokens, session)
    pub events: HashMap<(String, String), (f64, f64, String)>,
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, out),
            Ok(_) if p.extension().is_some_and(|x| x == "jsonl") => out.push(p),
            _ => {}
        }
    }
}

impl Scanner {
    pub fn scan(&mut self, root: &Path) {
        let cutoff = now() - KEEP_DAYS * 86400.0;
        let mut files = Vec::new();
        walk(root, &mut files);
        for path in files {
            let Ok(md) = fs::metadata(&path) else { continue };
            let size = md.len();
            let mtime = md.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            if !self.offsets.contains_key(&path) && mtime < cutoff {
                continue;
            }
            let mut off = *self.offsets.get(&path).unwrap_or(&0);
            if size < off {
                off = 0; // truncated or replaced: read again
                self.partial.remove(&path);
            }
            if size == off {
                continue;
            }
            let Ok(mut f) = fs::File::open(&path) else { continue };
            if f.seek(SeekFrom::Start(off)).is_err() {
                continue;
            }
            let mut buf = self.partial.remove(&path).unwrap_or_default();
            let mut chunk = vec![0u8; CHUNK];
            loop {
                let n = match f.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                off += n as u64;
                buf.extend_from_slice(&chunk[..n]);
                // complete lines only; keep the tail for later
                if let Some(last_nl) = buf.iter().rposition(|b| *b == b'\n') {
                    let rest = buf.split_off(last_nl + 1);
                    for line in buf.split(|b| *b == b'\n') {
                        self.ingest(line);
                    }
                    buf = rest;
                }
            }
            self.offsets.insert(path.clone(), off);
            if !buf.is_empty() {
                self.partial.insert(path, buf);
            }
        }
        self.events.retain(|_, v| v.0 >= cutoff);
    }

    fn ingest(&mut self, line: &[u8]) {
        if line.len() < 10 || !line.windows(7).any(|w| w == b"\"usage\"") {
            return;
        }
        let Ok(l) = serde_json::from_slice::<Line>(line) else { return };
        let Some(m) = l.message else { return };
        let Some(u) = m.usage else { return };
        let Some(ts) = parse_iso(l.timestamp.as_deref()) else { return };
        let tokens = u.input_tokens.unwrap_or(0.0) + u.output_tokens.unwrap_or(0.0) + u.cache_creation_input_tokens.unwrap_or(0.0);
        let key = (m.id.unwrap_or_default(), l.requestId.unwrap_or_default());
        let t = ts.timestamp() as f64 + ts.timestamp_subsec_millis() as f64 / 1000.0;
        self.events.insert(key, (t, tokens, l.sessionId.unwrap_or_default()));
    }

    pub fn compute(&self, at: f64) -> Stats {
        let today0 = Local::now().with_hour(0).and_then(|d| d.with_minute(0)).and_then(|d| d.with_second(0))
            .and_then(|d| d.with_nanosecond(0)).unwrap_or_else(Local::now);
        let nd = WEEKS * 7;
        // calendar days, oldest first; the last 7 are "this week" (today and the 6 before)
        let days: Vec<chrono::DateTime<Local>> = (0..nd).rev()
            .map(|i| Local.from_local_datetime(&(today0.naive_local() - CDuration::days(i as i64))).earliest().unwrap_or(today0))
            .collect();
        let starts: Vec<f64> = days.iter().map(|d| d.timestamp() as f64).collect();
        let mut dheat = vec![[0f64; 24]; nd];
        let (mut today, mut msgs, mut week, mut burn) = (0.0, 0u64, 0.0, 0.0);
        let mut active: HashSet<&str> = HashSet::new();
        for (ts, tok, sess) in self.events.values() {
            if *ts >= starts[0] {
                let di = starts.partition_point(|s| *s <= *ts) - 1;
                let hour = Local.timestamp_opt(*ts as i64, 0).single().map(|d| d.hour() as usize).unwrap_or(0);
                dheat[di][hour] += tok;
                if di >= nd - 7 {
                    week += tok;
                }
            }
            if *ts >= starts[nd - 1] {
                today += tok;
                msgs += 1;
            }
            if *ts >= at - 600.0 {
                burn += tok;
            }
            if *ts >= at - 300.0 && !sess.is_empty() {
                active.insert(sess);
            }
        }
        let mut weeks = Vec::new();
        for w in 0..WEEKS {
            let lo = nd - 7 * (w + 1);
            let blk = &days[lo..lo + 7];
            let heat = dheat[lo..lo + 7].to_vec();
            let total = heat.iter().map(|d| d.iter().sum::<f64>()).sum();
            weeks.push(Week {
                heat,
                day_labels: blk.iter().map(|d| d.format("%a").to_string()).collect(),
                day_dates: blk.iter().map(|d| format!("{} {}", d.format("%a %b"), d.format("%-d"))).collect(),
                label: if w == 0 {
                    "Last 7 days".into()
                } else {
                    format!("{} {} – {} {}", blk[0].format("%b"), blk[0].format("%-d"), blk[6].format("%b"), blk[6].format("%-d"))
                },
                total,
            });
        }
        Stats { today, msgs, week, burn: burn / 10.0, active: active.len(), weeks }
    }
}

/// Background thread: rescans every `logs_refresh_seconds` and publishes stats.
pub fn spawn(store: Arc<Store>, out: Arc<Mutex<Option<Stats>>>, on_update: Box<dyn Fn() + Send>) {
    std::thread::Builder::new()
        .name("logs".into())
        .spawn(move || {
            let mut sc = Scanner::default();
            let root = projects_dir();
            loop {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sc.scan(&root);
                    sc.compute(now())
                }));
                match r {
                    Ok(s) => {
                        let changed = out.lock().unwrap().as_ref() != Some(&s);
                        *out.lock().unwrap() = Some(s);
                        if changed {
                            on_update();
                        }
                    }
                    Err(_) => logf!("WARNING log scan failed"),
                }
                std::thread::sleep(std::time::Duration::from_secs(store.cfg_u64("logs_refresh_seconds").max(2)));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn line(mid: &str, rid: &str, inp: u64, out: u64, ts: &str) -> String {
        format!(r#"{{"timestamp":"{ts}","requestId":"{rid}","sessionId":"s1","message":{{"id":"{mid}","usage":{{"input_tokens":{inp},"output_tokens":{out},"cache_creation_input_tokens":100,"cache_read_input_tokens":999999}}}}}}"#)
    }

    #[test]
    fn counts_dedupes_and_waits_for_complete_lines() {
        let dir = std::env::temp_dir().join(format!("cuw-logs-{}", std::process::id()));
        let proj = dir.join("proj");
        fs::create_dir_all(&proj).unwrap();
        let ts = chrono::Utc::now().to_rfc3339();
        let p = proj.join("a.jsonl");
        let mut f = fs::File::create(&p).unwrap();
        writeln!(f, "{}", line("m1", "r1", 10, 20, &ts)).unwrap();
        writeln!(f, "{}", line("m1", "r1", 10, 20, &ts)).unwrap(); // streamed twice
        writeln!(f, r#"{{"type":"user","message":{{}}}}"#).unwrap();
        writeln!(f, "{}", line("m2", "r2", 1, 2, &ts)).unwrap();
        write!(f, "{}", line("m3", "r3", 5, 5, &ts)).unwrap(); // partial: no newline yet
        drop(f);
        let mut sc = Scanner::default();
        sc.scan(&dir);
        let st = sc.compute(now());
        assert_eq!(st.msgs, 2);
        assert_eq!(st.today, (10.0 + 20.0 + 100.0) + (1.0 + 2.0 + 100.0)); // cache reads excluded
        assert_eq!(st.active, 1);
        fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"\n").unwrap();
        sc.scan(&dir);
        assert_eq!(sc.compute(now()).msgs, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    fn noon(days_ago: i64) -> f64 {
        let d = Local::now().with_hour(12).unwrap().with_minute(0).unwrap() - CDuration::days(days_ago);
        d.timestamp() as f64
    }

    #[test]
    fn four_weeks_newest_first_with_correct_buckets() {
        let mut sc = Scanner::default();
        for (i, (t, tok)) in [(noon(0), 10.0), (noon(6), 20.0), (noon(7), 300.0), (noon(27), 4000.0), (noon(40), 5.0)].iter().enumerate() {
            sc.events.insert((i.to_string(), "r".into()), (*t, *tok, "s".into()));
        }
        let st = sc.compute(now());
        assert_eq!(st.weeks.iter().map(|w| w.total).collect::<Vec<_>>(), vec![30.0, 300.0, 0.0, 4000.0]);
        assert_eq!(st.weeks[0].label, "Last 7 days");
        assert_eq!(st.weeks[0].heat[6][12], 10.0);
        assert_eq!(st.weeks[0].heat[0][12], 20.0);
        assert_eq!(st.weeks[1].heat[6][12], 300.0);
        assert_eq!(st.week, 30.0);
        assert_eq!(st.today, 10.0);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// cargo test --release real_logs_timing -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_logs_timing() {
        let mut sc = Scanner::default();
        let t = std::time::Instant::now();
        sc.scan(&projects_dir());
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let st = sc.compute(now());
        let comp = t.elapsed();
        let t = std::time::Instant::now();
        sc.scan(&projects_dir());
        let rescan = t.elapsed();
        println!("events {} | first scan {:?} | compute {:?} | rescan {:?} | week totals {:?}",
                 sc.events.len(), first, comp, rescan, st.weeks.iter().map(|w| w.total).collect::<Vec<_>>());
    }
}
