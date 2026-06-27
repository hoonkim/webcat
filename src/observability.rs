use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ConsoleEntry {
    pub seq: u64,
    pub ts_ms: u64,
    pub level: String,
    pub text: String,
    pub url: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct NetworkEntry {
    pub seq: u64,
    pub ts_ms: u64,
    pub kind: String,
    pub method: Option<String>,
    pub url: String,
    pub status: Option<i64>,
    pub mime: Option<String>,
    pub request_id: String,
    pub request_body: Option<String>,
    pub response_body: Option<String>,
    pub response_body_base64: Option<bool>,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ConsolePage {
    pub entries: Vec<ConsoleEntry>,
    pub dropped: u64,
    pub latest_seq: u64,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct NetworkPage {
    pub entries: Vec<NetworkEntry>,
    pub dropped: u64,
    pub latest_seq: u64,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct PageViewport {
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Clone, Debug, serde::Serialize, schemars::JsonSchema)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
    pub viewport: Option<PageViewport>,
    pub loading: bool,
}

struct Buffers {
    console: VecDeque<ConsoleEntry>,
    network: VecDeque<NetworkEntry>,
    console_dropped: u64,
    network_dropped: u64,
    network_body_bytes: usize,
}

pub struct ObservabilityStore {
    cap: usize,
    network_body_budget_bytes: usize,
    seq: AtomicU64,
    inner: Mutex<Buffers>,
}

impl ObservabilityStore {
    pub fn new(cap: usize) -> Self {
        ObservabilityStore {
            cap,
            network_body_budget_bytes: NETWORK_BODY_BUDGET_BYTES,
            seq: AtomicU64::new(0),
            inner: Mutex::new(Buffers {
                console: VecDeque::new(),
                network: VecDeque::new(),
                console_dropped: 0,
                network_dropped: 0,
                network_body_bytes: 0,
            }),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn push_console(
        &self,
        level: String,
        text: String,
        url: Option<String>,
        line: Option<u32>,
    ) {
        let seq = self.next_seq();
        let mut g = self.inner.lock().unwrap();
        g.console.push_back(ConsoleEntry {
            seq,
            ts_ms: now_ms(),
            level,
            text,
            url,
            line,
        });
        while g.console.len() > self.cap {
            g.console.pop_front();
            g.console_dropped += 1;
        }
    }

    pub fn push_network(&self, mut e: NetworkEntry) {
        e.seq = self.next_seq();
        e.ts_ms = now_ms();
        let mut g = self.inner.lock().unwrap();
        g.network_body_bytes += network_body_bytes(&e);
        g.network.push_back(e);
        prune_network(&mut g, self.cap, self.network_body_budget_bytes);
    }

    pub fn attach_response_body(
        &self,
        request_id: &str,
        body: String,
        base64_encoded: bool,
    ) -> bool {
        let mut g = self.inner.lock().unwrap();
        let Some(idx) = g
            .network
            .iter()
            .rposition(|e| e.request_id == request_id && e.kind == "response")
        else {
            return false;
        };

        let old_bytes = network_body_bytes(&g.network[idx]);
        g.network_body_bytes = g.network_body_bytes.saturating_sub(old_bytes);
        let entry = &mut g.network[idx];
        entry.response_body = Some(body);
        entry.response_body_base64 = Some(base64_encoded);
        g.network_body_bytes += network_body_bytes(entry);
        prune_network(&mut g, self.cap, self.network_body_budget_bytes);
        true
    }

    pub fn console_since(&self, since_seq: u64, level: Option<&str>, limit: usize) -> ConsolePage {
        let g = self.inner.lock().unwrap();
        let latest_seq = self.seq.load(Ordering::Relaxed);
        let entries = g
            .console
            .iter()
            .filter(|e| e.seq > since_seq)
            .filter(|e| level.map_or(true, |l| e.level == l))
            .take(limit)
            .cloned()
            .collect();
        ConsolePage {
            entries,
            dropped: g.console_dropped,
            latest_seq,
        }
    }

    pub fn network_since(&self, since_seq: u64, status: Option<i64>, limit: usize) -> NetworkPage {
        let g = self.inner.lock().unwrap();
        let latest_seq = self.seq.load(Ordering::Relaxed);
        let entries = g
            .network
            .iter()
            .filter(|e| e.seq > since_seq)
            .filter(|e| status.map_or(true, |s| e.status == Some(s)))
            .take(limit)
            .cloned()
            .collect();
        NetworkPage {
            entries,
            dropped: g.network_dropped,
            latest_seq,
        }
    }
}

const NETWORK_BODY_BUDGET_BYTES: usize = 512 * 1024 * 1024;

fn prune_network(g: &mut Buffers, cap: usize, body_budget_bytes: usize) {
    while g.network.len() > cap || network_over_budget(g, body_budget_bytes) {
        let Some(entry) = g.network.pop_front() else {
            break;
        };
        g.network_body_bytes = g
            .network_body_bytes
            .saturating_sub(network_body_bytes(&entry));
        g.network_dropped += 1;
    }
}

fn network_over_budget(g: &Buffers, body_budget_bytes: usize) -> bool {
    g.network_body_bytes > body_budget_bytes && g.network.len() > 1
}

fn network_body_bytes(e: &NetworkEntry) -> usize {
    e.request_body.as_ref().map_or(0, |s| s.len())
        + e.response_body.as_ref().map_or(0, |s| s.len())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_body_budget(cap: usize, network_body_budget_bytes: usize) -> ObservabilityStore {
        ObservabilityStore {
            cap,
            network_body_budget_bytes,
            seq: AtomicU64::new(0),
            inner: Mutex::new(Buffers {
                console: VecDeque::new(),
                network: VecDeque::new(),
                console_dropped: 0,
                network_dropped: 0,
                network_body_bytes: 0,
            }),
        }
    }

    fn net(url: &str, status: Option<i64>) -> NetworkEntry {
        NetworkEntry {
            seq: 0,
            ts_ms: 0,
            kind: "response".into(),
            method: Some("GET".into()),
            url: url.into(),
            status,
            mime: None,
            request_id: "r".into(),
            request_body: None,
            response_body: None,
            response_body_base64: None,
        }
    }

    #[test]
    fn seq_increases_monotonically_across_buffers() {
        let s = ObservabilityStore::new(10);
        s.push_console("log".into(), "a".into(), None, None);
        s.push_network(net("https://x", Some(200)));
        s.push_console("log".into(), "b".into(), None, None);

        let c = s.console_since(0, None, 100);
        let n = s.network_since(0, None, 100);
        assert_eq!(c.entries[0].seq, 1);
        assert_eq!(n.entries[0].seq, 2);
        assert_eq!(c.entries[1].seq, 3);
    }

    #[test]
    fn since_seq_filters_old_entries() {
        let s = ObservabilityStore::new(10);
        s.push_console("log".into(), "a".into(), None, None);
        s.push_console("log".into(), "b".into(), None, None);
        let page = s.console_since(1, None, 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].text, "b");
    }

    #[test]
    fn ring_buffer_drops_oldest_and_counts() {
        let s = ObservabilityStore::new(2);
        for i in 0..5 {
            s.push_console("log".into(), format!("{i}"), None, None);
        }
        let page = s.console_since(0, None, 100);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.entries[0].text, "3");
        assert_eq!(page.dropped, 3);
    }

    #[test]
    fn level_filter_matches_exact() {
        let s = ObservabilityStore::new(10);
        s.push_console("error".into(), "e".into(), None, None);
        s.push_console("log".into(), "l".into(), None, None);
        let page = s.console_since(0, Some("error"), 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].text, "e");
    }

    #[test]
    fn status_filter_matches_network() {
        let s = ObservabilityStore::new(10);
        s.push_network(net("https://ok", Some(200)));
        s.push_network(net("https://err", Some(404)));
        let page = s.network_since(0, Some(404), 100);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].url, "https://err");
    }

    #[test]
    fn attaches_full_response_body_without_truncating() {
        let s = ObservabilityStore::new(10);
        s.push_network(net("https://graphql", Some(200)));
        let body = "x".repeat(1024 * 1024);
        assert!(s.attach_response_body("r", body.clone(), false));
        let page = s.network_since(0, None, 100);
        assert_eq!(page.entries[0].response_body.as_deref(), Some(body.as_str()));
        assert_eq!(page.entries[0].response_body_base64, Some(false));
    }

    #[test]
    fn body_budget_evicts_whole_old_entries() {
        let s = store_with_body_budget(10, 16 * 1024 * 1024);
        for i in 0..20 {
            let mut entry = net(&format!("https://x/{i}"), Some(200));
            entry.request_id = i.to_string();
            entry.response_body = Some("x".repeat(1024 * 1024));
            s.push_network(entry);
        }

        let page = s.network_since(0, None, 100);
        assert!(page.dropped > 0);
        assert!(page.entries.iter().all(|e| {
            e.response_body
                .as_ref()
                .is_some_and(|body| body.len() == 1024 * 1024)
        }));
    }
}
