use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    User,
    Background,
    Shutdown,
}

impl CancelReason {
    fn code(self) -> u8 {
        match self {
            Self::User => 1,
            Self::Background => 2,
            Self::Shutdown => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::User),
            2 => Some(Self::Background),
            3 => Some(Self::Shutdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<Inner>);

#[derive(Debug, Default)]
struct Inner {
    state: AtomicU8,
    notify: Notify,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.set(CancelReason::User);
    }

    pub fn cancel_for_background(&self) {
        self.set(CancelReason::Background);
    }

    pub fn shutdown(&self) {
        self.set(CancelReason::Shutdown);
    }

    fn set(&self, reason: CancelReason) {
        if self.0.state.swap(reason.code(), Ordering::SeqCst) == 0 {
            self.0.notify.notify_waiters();
        }
    }

    pub fn reason(&self) -> Option<CancelReason> {
        CancelReason::from_code(self.0.state.load(Ordering::SeqCst))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.state.load(Ordering::SeqCst) != 0
    }

    pub fn take_background(&self) -> bool {
        if self.reason() == Some(CancelReason::Background) {
            self.reset();
            true
        } else {
            false
        }
    }

    pub fn reset(&self) {
        self.0.state.store(0, Ordering::SeqCst);
    }

    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            self.0.notify.notified().await;
        }
    }
}
