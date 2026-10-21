use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use netconf_async::Connection;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::mcp_params;

pub(crate) const MAX_SUBSCRIPTIONS: usize = 4;
pub(crate) const IDLE: Duration = Duration::from_secs(300);
pub(crate) const MAX_WAIT_MS: u64 = 300_000;
const MAX_BUFFER: usize = 256;

const _: () = assert!(MAX_WAIT_MS == IDLE.as_millis() as u64);

struct Subscription {
    notifications: Arc<Mutex<VecDeque<String>>>,
    notify: Arc<Notify>,
    cancel: CancellationToken,
    last_pull: Instant,
    dead: Arc<AtomicBool>,
    in_use: Arc<AtomicUsize>,
}

struct InUseGuard(Arc<AtomicUsize>);

impl Drop for InUseGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

/// Listen-only notification sessions owned by one MCP session.
pub(crate) struct Subscriptions {
    inner: Mutex<HashMap<String, Subscription>>,
}

impl Default for Subscriptions {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl Drop for Subscriptions {
    fn drop(&mut self) {
        if let Ok(mut map) = self.inner.try_lock() {
            for sub in map.values() {
                sub.cancel.cancel();
            }
            map.clear();
        }
    }
}

impl Subscriptions {
    pub(crate) async fn sweep(&self) {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        map.retain(|_, sub| {
            if sub.in_use.load(Ordering::Acquire) > 0 {
                return true;
            }
            let idle = now.duration_since(sub.last_pull) > IDLE;
            if idle {
                sub.cancel.cancel();
                return false;
            }
            if sub.dead.load(Ordering::Acquire) {
                let empty = sub
                    .notifications
                    .try_lock()
                    .map(|buf| buf.is_empty())
                    .unwrap_or(true);
                if empty {
                    sub.cancel.cancel();
                    return false;
                }
            }
            true
        });
    }

    pub(crate) async fn insert(&self, mut conn: Connection) -> Result<String, rmcp::ErrorData> {
        self.sweep().await;
        let mut map = self.inner.lock().await;
        if map.len() >= MAX_SUBSCRIPTIONS {
            return Err(mcp_params(format!(
                "too many subscriptions (max {MAX_SUBSCRIPTIONS})"
            )));
        }
        let id = Uuid::new_v4().to_string();
        let notifications = Arc::new(Mutex::new(VecDeque::new()));
        let notify = Arc::new(Notify::new());
        let cancel = CancellationToken::new();
        let task_notes = notifications.clone();
        let task_notify = notify.clone();
        let task_cancel = cancel.clone();
        let dead = Arc::new(AtomicBool::new(false));
        let task_dead = dead.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    incoming = conn.recv_notification() => {
                        match incoming {
                            Ok(xml) => {
                                let mut buf = task_notes.lock().await;
                                if buf.len() >= MAX_BUFFER {
                                    buf.pop_front();
                                }
                                buf.push_back(xml);
                                drop(buf);
                                task_notify.notify_waiters();
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
            task_dead.store(true, Ordering::Release);
            task_notify.notify_waiters();
            let _ = conn.close_session().await;
        });
        map.insert(
            id.clone(),
            Subscription {
                notifications,
                notify,
                cancel,
                last_pull: Instant::now(),
                dead,
                in_use: Arc::new(AtomicUsize::new(0)),
            },
        );
        Ok(id)
    }

    pub(crate) async fn pull(
        &self,
        id: &str,
        wait_ms: u64,
    ) -> Result<Vec<String>, rmcp::ErrorData> {
        if wait_ms > MAX_WAIT_MS {
            return Err(mcp_params(format!(
                "wait_ms must be <= {MAX_WAIT_MS} (idle sweep is {}s)",
                IDLE.as_secs()
            )));
        }
        self.sweep().await;
        let (notes, notify, dead, _in_use) = {
            let mut map = self.inner.lock().await;
            let sub = map
                .get_mut(id)
                .ok_or_else(|| mcp_params(format!("unknown subscription_id {id}")))?;
            if sub.dead.load(Ordering::Acquire) {
                return take_dead(&mut map, id);
            }
            sub.last_pull = Instant::now();
            sub.in_use.fetch_add(1, Ordering::AcqRel);
            (
                sub.notifications.clone(),
                sub.notify.clone(),
                sub.dead.clone(),
                InUseGuard(sub.in_use.clone()),
            )
        };
        if wait_ms > 0 {
            let wait = async {
                loop {
                    // Create the waiter before reading the buffer so a
                    // notify_waiters() between the check and await is not lost.
                    let notified = notify.notified();
                    {
                        let buf = notes.lock().await;
                        if !buf.is_empty() || dead.load(Ordering::Acquire) {
                            return;
                        }
                    }
                    notified.await;
                }
            };
            let _ = tokio::time::timeout(Duration::from_millis(wait_ms), wait).await;
        }
        let drained = {
            let mut buf = notes.lock().await;
            buf.drain(..).collect::<Vec<_>>()
        };
        {
            let mut map = self.inner.lock().await;
            if let Some(sub) = map.get_mut(id) {
                sub.last_pull = Instant::now();
            }
            if dead.load(Ordering::Acquire) {
                if let Some(sub) = map.remove(id) {
                    sub.cancel.cancel();
                }
                if drained.is_empty() {
                    return Err(mcp_params(format!("subscription {id} ended")));
                }
            }
        }
        Ok(drained)
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<(), rmcp::ErrorData> {
        let mut map = self.inner.lock().await;
        let sub = map
            .remove(id)
            .ok_or_else(|| mcp_params(format!("unknown subscription_id {id}")))?;
        sub.cancel.cancel();
        Ok(())
    }
}

fn take_dead(
    map: &mut HashMap<String, Subscription>,
    id: &str,
) -> Result<Vec<String>, rmcp::ErrorData> {
    let Some(sub) = map.remove(id) else {
        return Err(mcp_params(format!("unknown subscription_id {id}")));
    };
    sub.cancel.cancel();
    let leftover = sub
        .notifications
        .try_lock()
        .map(|mut buf| buf.drain(..).collect::<Vec<_>>());
    match leftover {
        Ok(items) if items.is_empty() => Err(mcp_params(format!("subscription {id} ended"))),
        Ok(items) => Ok(items),
        Err(_) => Err(mcp_params(format!("subscription {id} ended"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dead_subscription_frees_slot() {
        let subs = Subscriptions::default();
        {
            let mut map = subs.inner.lock().await;
            for _ in 0..MAX_SUBSCRIPTIONS {
                map.insert(
                    Uuid::new_v4().to_string(),
                    Subscription {
                        notifications: Arc::new(Mutex::new(VecDeque::new())),
                        notify: Arc::new(Notify::new()),
                        cancel: CancellationToken::new(),
                        last_pull: Instant::now(),
                        dead: Arc::new(AtomicBool::new(true)),
                        in_use: Arc::new(AtomicUsize::new(0)),
                    },
                );
            }
        }
        subs.sweep().await;
        assert!(subs.inner.lock().await.is_empty());
    }

    #[tokio::test]
    async fn pull_of_dead_sub_returns_leftover_then_frees() {
        let subs = Subscriptions::default();
        let id = {
            let mut map = subs.inner.lock().await;
            let id = "dead-sub".to_string();
            let mut buf = VecDeque::new();
            buf.push_back("<notification/>".into());
            map.insert(
                id.clone(),
                Subscription {
                    notifications: Arc::new(Mutex::new(buf)),
                    notify: Arc::new(Notify::new()),
                    cancel: CancellationToken::new(),
                    last_pull: Instant::now(),
                    dead: Arc::new(AtomicBool::new(true)),
                    in_use: Arc::new(AtomicUsize::new(0)),
                },
            );
            id
        };
        let got = subs.pull(&id, 0).await.unwrap();
        assert_eq!(got, vec!["<notification/>".to_string()]);
        assert!(subs.inner.lock().await.is_empty());
        let err = subs.pull(&id, 0).await.unwrap_err();
        assert!(err.message.to_string().contains("unknown"), "{err:?}");
    }

    async fn insert_fake(
        subs: &Subscriptions,
        dead: bool,
    ) -> (
        String,
        Arc<Mutex<VecDeque<String>>>,
        Arc<Notify>,
        Arc<AtomicBool>,
    ) {
        let mut map = subs.inner.lock().await;
        let id = Uuid::new_v4().to_string();
        let notifications = Arc::new(Mutex::new(VecDeque::new()));
        let notify = Arc::new(Notify::new());
        let flag = Arc::new(AtomicBool::new(dead));
        map.insert(
            id.clone(),
            Subscription {
                notifications: notifications.clone(),
                notify: notify.clone(),
                cancel: CancellationToken::new(),
                last_pull: Instant::now(),
                dead: flag.clone(),
                in_use: Arc::new(AtomicUsize::new(0)),
            },
        );
        (id, notifications, notify, flag)
    }

    #[tokio::test]
    async fn pull_wait_wakes_on_notify() {
        let subs = Arc::new(Subscriptions::default());
        let (id, notes, notify, _) = insert_fake(&subs, false).await;
        let pull = {
            let subs = subs.clone();
            let id = id.clone();
            tokio::spawn(async move { subs.pull(&id, 5_000).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        notes.lock().await.push_back("<notification/>".into());
        notify.notify_waiters();
        let got = tokio::time::timeout(Duration::from_millis(500), pull)
            .await
            .expect("pull waited out")
            .unwrap()
            .unwrap();
        assert_eq!(got, vec!["<notification/>".to_string()]);
    }

    #[tokio::test]
    async fn pull_wait_wakes_when_subscription_dies() {
        let subs = Arc::new(Subscriptions::default());
        let (id, _, notify, dead) = insert_fake(&subs, false).await;
        let pull = {
            let subs = subs.clone();
            let id = id.clone();
            tokio::spawn(async move { subs.pull(&id, 5_000).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        dead.store(true, Ordering::Release);
        notify.notify_waiters();
        let err = tokio::time::timeout(Duration::from_millis(500), pull)
            .await
            .expect("pull waited out")
            .unwrap()
            .unwrap_err();
        assert!(err.message.to_string().contains("ended"), "{err:?}");
    }

    #[tokio::test]
    async fn sweep_skips_in_use_even_when_idle() {
        let subs = Subscriptions::default();
        let id = {
            let mut map = subs.inner.lock().await;
            let id = "busy".to_string();
            map.insert(
                id.clone(),
                Subscription {
                    notifications: Arc::new(Mutex::new(VecDeque::new())),
                    notify: Arc::new(Notify::new()),
                    cancel: CancellationToken::new(),
                    last_pull: Instant::now() - IDLE - Duration::from_secs(1),
                    dead: Arc::new(AtomicBool::new(false)),
                    in_use: Arc::new(AtomicUsize::new(1)),
                },
            );
            id
        };
        subs.sweep().await;
        assert!(subs.inner.lock().await.contains_key(&id));
        {
            let map = subs.inner.lock().await;
            map.get(&id).unwrap().in_use.store(0, Ordering::Release);
        }
        subs.sweep().await;
        assert!(subs.inner.lock().await.is_empty());
    }

    #[tokio::test]
    async fn wait_ms_over_idle_is_rejected() {
        let subs = Subscriptions::default();
        let err = subs.pull("x", MAX_WAIT_MS + 1).await.unwrap_err();
        assert!(err.message.to_string().contains("wait_ms"), "{err:?}");
    }
}
