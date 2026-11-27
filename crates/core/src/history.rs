//! `history.jsonl` (AC-HIST-1/3/5): append-only JSON lines, newest 500 kept.
//!
//! Only lightweight summaries and byte offsets live in memory; full entries
//! (which may carry up to 1 MB of body each) are read from disk on demand.
//! The file is compacted to the newest 500 entries on open and whenever it
//! grows past 500 + [`COMPACT_SLACK`] lines.

use crate::model::{ErrorKind, GrpcCallSummary, HistoryEntry, WsSessionSummary};
use crate::storage::{StoreError, atomic_write};
use serde::Serialize;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

pub const HISTORY_LIMIT: usize = 500;
pub const COMPACT_SLACK: usize = 50;

/// What the Activity list needs per row (AC-HIST-2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySummary {
    pub id: String,
    pub timestamp: i64,
    pub method: String,
    pub resolved_url: String,
    pub status: Option<u16>,
    pub time_ms: Option<u64>,
    pub error_kind: Option<ErrorKind>,
    pub source_request_id: Option<String>,
    /// WebSocket / Socket.IO sessions: duration and message counts.
    pub ws: Option<WsSessionSummary>,
    /// gRPC calls: kind, counts, duration and status, without the response,
    /// headers, trailers and status details (AC-GRPC-10).
    pub grpc: Option<GrpcCallSummary>,
}

impl HistorySummary {
    pub fn from_entry(e: &HistoryEntry) -> Self {
        HistorySummary {
            id: e.id.clone(),
            timestamp: e.timestamp,
            method: e.method.clone(),
            resolved_url: e.resolved_url.clone(),
            status: e.response.as_ref().map(|r| r.status),
            time_ms: e.response.as_ref().map(|r| r.time_ms),
            error_kind: e.error.as_ref().map(|e| e.kind),
            source_request_id: e.source_request_id.clone(),
            ws: e.ws.clone(),
            grpc: e.grpc.as_ref().map(|g| GrpcCallSummary {
                response: None,
                response_truncated: false,
                headers: None,
                trailers: None,
                status: g.status.as_ref().map(|s| crate::model::GrpcStatus {
                    details: Vec::new(),
                    ..s.clone()
                }),
                ..g.clone()
            }),
        }
    }

    /// AC-HIST-3: matches URL, method or status code, case-insensitively.
    pub fn matches(&self, filter: &str) -> bool {
        let f = filter.trim().to_ascii_lowercase();
        if f.is_empty() {
            return true;
        }
        self.resolved_url.to_ascii_lowercase().contains(&f)
            || self.method.to_ascii_lowercase().contains(&f)
            || self.status.is_some_and(|s| s.to_string().contains(&f))
    }
}

#[derive(Debug, Clone)]
struct Slot {
    summary: HistorySummary,
    offset: u64,
    len: u64,
}

pub struct History {
    path: PathBuf,
    /// Oldest first.
    slots: Vec<Slot>,
    file_len: u64,
}

impl History {
    pub fn open(path: PathBuf) -> Result<History, StoreError> {
        let mut h = History {
            path,
            slots: Vec::new(),
            file_len: 0,
        };
        let data = match std::fs::read(&h.path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(h),
            Err(e) => return Err(e.into()),
        };
        let mut offset = 0u64;
        let mut bad_lines = 0usize;
        for line in data.split(|b| *b == b'\n') {
            let len = line.len() as u64;
            if !line.iter().all(|b| b.is_ascii_whitespace()) {
                match serde_json::from_slice::<HistoryEntry>(line) {
                    Ok(e) => h.slots.push(Slot {
                        summary: HistorySummary::from_entry(&e),
                        offset,
                        len,
                    }),
                    Err(_) => bad_lines += 1,
                }
            }
            offset += len + 1;
        }
        h.file_len = data.len() as u64;
        if h.slots.len() > HISTORY_LIMIT || bad_lines > 0 {
            h.compact()?;
        }
        Ok(h)
    }

    /// Newest first, at most 500.
    pub fn list(&self) -> Vec<HistorySummary> {
        self.slots
            .iter()
            .rev()
            .take(HISTORY_LIMIT)
            .map(|s| s.summary.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.slots.len().min(HISTORY_LIMIT)
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Append an entry (AC-HIST-1).
    pub fn append(&mut self, entry: &HistoryEntry) -> Result<HistorySummary, StoreError> {
        let mut line = serde_json::to_vec(entry).map_err(|e| StoreError::Io(e.to_string()))?;
        let len = line.len() as u64;
        line.push(b'\n');
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let offset = f.seek(SeekFrom::End(0))?;
        f.write_all(&line)?;
        f.sync_data()?;
        self.file_len = offset + line.len() as u64;
        let summary = HistorySummary::from_entry(entry);
        self.slots.push(Slot {
            summary: summary.clone(),
            offset,
            len,
        });
        if self.slots.len() > HISTORY_LIMIT + COMPACT_SLACK {
            self.compact()?;
        }
        Ok(summary)
    }

    fn read_slot(&self, slot: &Slot) -> Result<HistoryEntry, StoreError> {
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(slot.offset))?;
        let mut buf = vec![0u8; slot.len as usize];
        f.read_exact(&mut buf)?;
        serde_json::from_slice(&buf)
            .map_err(|e| StoreError::Io(format!("history entry unreadable: {e}")))
    }

    /// Full entry, including the stored response (AC-HIST-4).
    pub fn get(&self, id: &str) -> Result<HistoryEntry, StoreError> {
        let slot = self
            .slots
            .iter()
            .find(|s| s.summary.id == id)
            .ok_or_else(|| StoreError::NotFound(format!("History entry {id}")))?;
        self.read_slot(slot)
    }

    /// Delete one entry (AC-HIST-5).
    pub fn delete(&mut self, id: &str) -> Result<(), StoreError> {
        if !self.slots.iter().any(|s| s.summary.id == id) {
            return Err(StoreError::NotFound(format!("History entry {id}")));
        }
        self.rewrite(|s| s.summary.id != id)
    }

    /// Clear everything (AC-HIST-5).
    pub fn clear(&mut self) -> Result<(), StoreError> {
        atomic_write(&self.path, b"")?;
        self.slots.clear();
        self.file_len = 0;
        Ok(())
    }

    /// Keep only the newest 500 entries on disk.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        let drop_n = self.slots.len().saturating_sub(HISTORY_LIMIT);
        let keep_from: Vec<String> = self.slots[drop_n..]
            .iter()
            .map(|s| s.summary.id.clone())
            .collect();
        self.rewrite(|s| keep_from.contains(&s.summary.id))
    }

    fn rewrite(&mut self, keep: impl Fn(&Slot) -> bool) -> Result<(), StoreError> {
        let data = std::fs::read(&self.path).unwrap_or_default();
        let mut out = Vec::with_capacity(data.len());
        let mut slots = Vec::new();
        for slot in self.slots.iter().filter(|s| keep(s)) {
            let start = slot.offset as usize;
            let end = start + slot.len as usize;
            let Some(line) = data.get(start..end) else {
                continue;
            };
            let offset = out.len() as u64;
            out.extend_from_slice(line);
            out.push(b'\n');
            slots.push(Slot {
                summary: slot.summary.clone(),
                offset,
                len: slot.len,
            });
        }
        atomic_write(&self.path, &out)?;
        self.file_len = out.len() as u64;
        self.slots = slots;
        Ok(())
    }
}

/// Filter summaries (AC-HIST-3).
pub fn filter_history(items: &[HistorySummary], filter: &str) -> Vec<HistorySummary> {
    items
        .iter()
        .filter(|s| s.matches(filter))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HistoryResponse, Request, SendError, new_id};

    fn entry(i: usize, method: &str, status: Option<u16>) -> HistoryEntry {
        HistoryEntry {
            id: new_id(),
            timestamp: i as i64,
            source_request_id: None,
            request: Request::default(),
            resolved_url: format!("https://api.dev/items/{i}"),
            method: method.into(),
            response: status.map(|s| HistoryResponse {
                status: s,
                status_text: String::new(),
                time_ms: 5,
                size_bytes: 2,
                headers: vec![],
                body: "ok".into(),
                truncated: false,
                body_encoding: Default::default(),
            }),
            error: status.is_none().then(|| SendError {
                kind: ErrorKind::Cancelled,
                message: "cancelled".into(),
            }),
            ws: None,
            grpc: None,
        }
    }

    #[test]
    fn ac_hist_1_newest_500_kept_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut h = History::open(path.clone()).unwrap();
        for i in 0..620 {
            h.append(&entry(i, "GET", Some(200))).unwrap();
        }
        assert_eq!(h.list().len(), 500);
        assert_eq!(h.list()[0].timestamp, 619);
        assert_eq!(h.list()[499].timestamp, 120);
        let h2 = History::open(path.clone()).unwrap();
        assert_eq!(h2.list().len(), 500);
        let lines = std::fs::read_to_string(&path).unwrap().lines().count();
        assert_eq!(lines, 500, "compacted on startup");
        let newest = h2.list()[0].clone();
        assert_eq!(h2.get(&newest.id).unwrap().timestamp, 619);
    }

    #[test]
    fn ac_hist_3_filter_by_url_method_status() {
        let items: Vec<HistorySummary> = vec![
            HistorySummary::from_entry(&entry(1, "GET", Some(200))),
            HistorySummary::from_entry(&entry(2, "QUERY", Some(404))),
            HistorySummary::from_entry(&entry(3, "POST", None)),
        ];
        assert_eq!(filter_history(&items, "404").len(), 1);
        assert_eq!(filter_history(&items, "query")[0].method, "QUERY");
        assert_eq!(filter_history(&items, "items/3").len(), 1);
        assert_eq!(filter_history(&items, "").len(), 3);
        assert_eq!(filter_history(&items, "nope").len(), 0);
    }

    #[test]
    fn ac_hist_5_delete_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut h = History::open(path.clone()).unwrap();
        let a = entry(1, "GET", Some(200));
        let b = entry(2, "GET", None);
        h.append(&a).unwrap();
        h.append(&b).unwrap();
        h.delete(&a.id).unwrap();
        assert_eq!(h.list().len(), 1);
        assert_eq!(
            h.get(&b.id).unwrap().error.unwrap().kind,
            ErrorKind::Cancelled
        );
        let h2 = History::open(path.clone()).unwrap();
        assert_eq!(h2.list().len(), 1);
        h.clear().unwrap();
        assert!(h.list().is_empty());
        assert!(History::open(path).unwrap().list().is_empty());
    }

    #[test]
    fn corrupt_lines_are_dropped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut h = History::open(path.clone()).unwrap();
        h.append(&entry(1, "GET", Some(200))).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"{ broken\n").unwrap();
        drop(f);
        let h2 = History::open(path).unwrap();
        assert_eq!(h2.list().len(), 1);
    }
}
