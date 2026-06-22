use tokio::sync::watch;

#[derive(Debug, Clone)]
pub struct Frame {
    pub jpeg: Vec<u8>,
}

pub struct FrameTx {
    inner: watch::Sender<Option<Frame>>,
}

pub struct FrameRx {
    inner: watch::Receiver<Option<Frame>>,
}

pub fn frame_channel() -> (FrameTx, FrameRx) {
    let (tx, rx) = watch::channel(None);
    (FrameTx { inner: tx }, FrameRx { inner: rx })
}

impl FrameTx {
    pub fn send(&self, frame: Frame) {
        // Overwrites the stored value; lagging receivers only see the latest.
        let _ = self.inner.send(Some(frame));
    }
}

impl FrameRx {
    pub async fn recv(&mut self) -> Option<Frame> {
        loop {
            // Wait for a change from the current value.
            if self.inner.changed().await.is_err() {
                return None; // all senders dropped
            }
            let val = self.inner.borrow_and_update().clone();
            if let Some(f) = val {
                return Some(f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recv_gets_latest_after_multiple_sends() {
        let (tx, mut rx) = frame_channel();
        tx.send(Frame { jpeg: vec![1] });
        tx.send(Frame { jpeg: vec![2] });
        tx.send(Frame { jpeg: vec![3] });
        let f = rx.recv().await.unwrap();
        assert_eq!(f.jpeg, vec![3]); // coalesced to most recent
    }

    #[tokio::test]
    async fn recv_returns_none_when_tx_dropped() {
        let (tx, mut rx) = frame_channel();
        drop(tx);
        assert!(rx.recv().await.is_none());
    }
}
