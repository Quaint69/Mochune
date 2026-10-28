//! Reachability of YouTube, not a claim about all Internet services.
use super::*;
use std::sync::atomic::AtomicU8;
pub(super) struct Connection {
    state: AtomicU8,
    retry: mpsc::SyncSender<()>,
    receiver: Mutex<Option<mpsc::Receiver<()>>>,
    forced: bool,
}
impl Connection {
    pub fn new(forced: bool) -> Arc<Self> {
        let (tx, rx) = mpsc::sync_channel(1);
        Arc::new(Self {
            state: AtomicU8::new(if forced { 2 } else { 0 }),
            retry: tx,
            receiver: Mutex::new(Some(rx)),
            forced,
        })
    }
    pub fn offline(&self) -> bool {
        self.state.load(Ordering::Acquire) == 2
    }
    pub fn retry(&self) {
        let _ = self.retry.try_send(());
    }
    pub fn install(self: &Arc<Self>, w: &SetupWindow) {
        w.set_offline(self.offline());
        w.set_connection_status(
            if self.offline() {
                "Offline · cached music available"
            } else {
                "Checking connection…"
            }
            .into(),
        );
        let weak = w.as_weak();
        let connection = self.clone();
        let rx = self.receiver.lock().unwrap().take().unwrap();
        thread::spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(3)))
                .max_idle_connections(0)
                .build()
                .into();
            loop {
                let offline = connection.forced
                    || agent
                        .head("https://www.youtube.com/generate_204")
                        .call()
                        .is_err();
                connection
                    .state
                    .store(if offline { 2 } else { 1 }, Ordering::Release);
                let sent = weak.upgrade_in_event_loop(move |w| {
                    w.set_offline(offline);
                    w.set_connection_status(
                        if offline {
                            "Offline · cached music still plays"
                        } else {
                            "Online"
                        }
                        .into(),
                    );
                });
                if sent.is_err() {
                    break;
                }
                if rx
                    .recv_timeout(Duration::from_secs(if offline { 10 } else { 25 }))
                    .is_err_and(|e| matches!(e, mpsc::RecvTimeoutError::Disconnected))
                {
                    break;
                }
            }
        });
        let connection = self.clone();
        w.on_retry_connection(move || connection.retry());
    }
}
