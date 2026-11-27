//! WebSocket runtime (AC-WS-*): one task per connection runs
//! `reqharbor_core::ws::run` and streams its frames to the webview over the
//! connection's `ipc::Channel`. The registry maps a client-chosen
//! `connectionId` to the task's command sender; it is free of Tauri types so
//! it can be unit-tested.

use reqharbor_core::CancellationToken;
use reqharbor_core::ws::{OutMessage, WsCommand};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use tokio::sync::{mpsc, oneshot};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

struct Entry {
    serial: u64,
    tx: mpsc::UnboundedSender<WsCommand>,
    cancel: CancellationToken,
    /// Project whose variables resolve the messages sent on it.
    project_id: Option<String>,
    socket_io: bool,
}

/// connectionId -> live session.
#[derive(Default)]
pub struct WsRegistry {
    serial: AtomicU64,
    sessions: Mutex<HashMap<String, Entry>>,
}

/// Handle returned by [`WsRegistry::register`].
pub struct Registered {
    pub serial: u64,
    pub rx: mpsc::UnboundedReceiver<WsCommand>,
    pub cancel: CancellationToken,
}

/// Why a send could not be queued.
#[derive(Debug, PartialEq, Eq)]
pub enum SendRefused {
    NotConnected,
    Session(String),
}

impl WsRegistry {
    /// Register a session. Re-using an id closes the previous session.
    pub fn register(&self, id: &str, project_id: Option<String>, socket_io: bool) -> Registered {
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        let entry = Entry {
            serial,
            tx,
            cancel: cancel.clone(),
            project_id,
            socket_io,
        };
        if let Some(old) = lock(&self.sessions).insert(id.to_string(), entry) {
            let _ = old.tx.send(WsCommand::Close {
                code: None,
                reason: String::new(),
            });
        }
        Registered { serial, rx, cancel }
    }

    /// (project id, Socket.IO mode) of a live session.
    pub fn info(&self, id: &str) -> Option<(Option<String>, bool)> {
        lock(&self.sessions)
            .get(id)
            .map(|e| (e.project_id.clone(), e.socket_io))
    }

    /// Queue a message; resolves with the Socket.IO ack id (if requested)
    /// once it is on the wire.
    pub async fn send(&self, id: &str, msg: OutMessage) -> Result<Option<u64>, SendRefused> {
        let (reply, rx) = oneshot::channel();
        {
            let map = lock(&self.sessions);
            let e = map.get(id).ok_or(SendRefused::NotConnected)?;
            e.tx.send(WsCommand::Send(msg, reply))
                .map_err(|_| SendRefused::NotConnected)?;
        }
        match rx.await {
            Ok(r) => r.map_err(SendRefused::Session),
            Err(_) => Err(SendRefused::NotConnected),
        }
    }

    pub fn ping(&self, id: &str, data: Vec<u8>) -> bool {
        lock(&self.sessions)
            .get(id)
            .is_some_and(|e| e.tx.send(WsCommand::Ping(data)).is_ok())
    }

    /// Ask a session to close. False when nothing is open under `id`.
    pub fn close(&self, id: &str, code: Option<u16>, reason: String) -> bool {
        lock(&self.sessions)
            .get(id)
            .is_some_and(|e| e.tx.send(WsCommand::Close { code, reason }).is_ok())
    }

    /// Forget a finished session (unless a newer one re-used the id).
    pub fn finish(&self, id: &str, serial: u64) {
        let mut map = lock(&self.sessions);
        if map.get(id).is_some_and(|e| e.serial == serial) {
            map.remove(id);
        }
    }

    /// App exit: every session sends a 1001 close and ends.
    pub fn close_all(&self) -> usize {
        let map = lock(&self.sessions);
        for e in map.values() {
            e.cancel.cancel();
        }
        map.len()
    }

    pub fn len(&self) -> usize {
        lock(&self.sessions).len()
    }
}

/// The session's frame sink: forwards each frame with `send` (the webview
/// channel) and, on the first failure (the webview reloaded or went away),
/// cancels the session instead of letting it run and reconnect with no UI.
pub fn channel_emitter<T, E>(
    send: impl Fn(T) -> Result<(), E> + Send + Sync,
    cancel: CancellationToken,
) -> impl Fn(T) + Send + Sync {
    let failed = AtomicBool::new(false);
    move |f| {
        if failed.load(Ordering::Relaxed) {
            return;
        }
        if send(f).is_err() {
            failed.store(true, Ordering::Relaxed);
            cancel.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqharbor_core::ws::{Dir, FrameKind, WsFrame};

    #[test]
    fn a_failing_channel_cancels_the_session_once() {
        let cancel = CancellationToken::new();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let c2 = calls.clone();
        let emit = channel_emitter(
            move |_f: WsFrame| {
                let n = c2.fetch_add(1, Ordering::SeqCst);
                if n == 0 { Ok(()) } else { Err("webview gone") }
            },
            cancel.clone(),
        );
        let frame = || WsFrame::new(Dir::In, FrameKind::Text, "x", 1);
        emit(frame());
        assert!(!cancel.is_cancelled(), "a delivered frame changes nothing");
        emit(frame());
        assert!(
            cancel.is_cancelled(),
            "the first failure cancels the session"
        );
        emit(frame());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "no more sends after a failure"
        );
    }

    #[tokio::test]
    async fn ac_ws_3_registry_routes_commands_and_forgets_finished_sessions() {
        let r = WsRegistry::default();
        assert_eq!(
            r.send("nope", OutMessage::Text("x".into())).await,
            Err(SendRefused::NotConnected)
        );
        assert!(!r.close("nope", None, String::new()));
        let mut a = r.register("c1", Some("p".into()), true);
        assert_eq!(r.info("c1"), Some((Some("p".into()), true)));
        // A fake session answers the send with an ack id.
        let task = tokio::spawn(async move {
            match a.rx.recv().await {
                Some(WsCommand::Send(OutMessage::Text(t), reply)) => {
                    assert_eq!(t, "hi");
                    let _ = reply.send(Ok(Some(7)));
                }
                other => panic!("{other:?}"),
            }
            assert!(matches!(a.rx.recv().await, Some(WsCommand::Ping(_))));
            assert!(matches!(
                a.rx.recv().await,
                Some(WsCommand::Close {
                    code: Some(4000),
                    ..
                })
            ));
            a
        });
        assert_eq!(
            r.send("c1", OutMessage::Text("hi".into())).await,
            Ok(Some(7))
        );
        assert!(r.ping("c1", vec![]));
        assert!(r.close("c1", Some(4000), "bye".into()));
        let a = task.await.unwrap();
        // Re-using the id closes the old session; the stale finish is ignored.
        let mut old_rx = a.rx;
        let b = r.register("c1", None, false);
        assert!(matches!(
            old_rx.recv().await,
            Some(WsCommand::Close { code: None, .. })
        ));
        r.finish("c1", a.serial);
        assert_eq!(r.len(), 1);
        assert_eq!(r.close_all(), 1);
        assert!(b.cancel.is_cancelled());
        r.finish("c1", b.serial);
        assert_eq!(r.len(), 0);
    }

    #[tokio::test]
    async fn a_session_that_refuses_reports_its_reason() {
        let r = WsRegistry::default();
        let mut s = r.register("c", None, false);
        tokio::spawn(async move {
            if let Some(WsCommand::Send(_, reply)) = s.rx.recv().await {
                let _ = reply.send(Err("Not connected yet".into()));
            }
        });
        assert_eq!(
            r.send("c", OutMessage::Text("x".into())).await,
            Err(SendRefused::Session("Not connected yet".into()))
        );
    }
}
