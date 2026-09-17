use std::sync::{Arc, Mutex, MutexGuard};

struct State<T> {
    pending: Option<T>,
    receiver_alive: bool,
}

pub(super) struct LatestPublisher<T> {
    shared: Arc<Mutex<State<T>>>,
}

pub(super) struct LatestReceiver<T> {
    shared: Arc<Mutex<State<T>>>,
}

pub(super) fn channel<T>() -> (LatestPublisher<T>, LatestReceiver<T>) {
    let shared = Arc::new(Mutex::new(State {
        pending: None,
        receiver_alive: true,
    }));
    (
        LatestPublisher {
            shared: Arc::clone(&shared),
        },
        LatestReceiver { shared },
    )
}

impl<T> LatestPublisher<T> {
    /// Replaces any pending snapshot without waiting for the receiver to consume it.
    ///
    /// Returns `false` once the receiver has been dropped.
    pub(super) fn publish(&self, value: T) -> bool {
        let mut state = lock(&self.shared);
        if !state.receiver_alive {
            return false;
        }

        let replaced = state.pending.replace(value);
        drop(state);
        drop(replaced);
        true
    }
}

impl<T> LatestReceiver<T> {
    pub(super) fn take_latest(&self) -> Option<T> {
        lock(&self.shared).pending.take()
    }
}

impl<T> Drop for LatestReceiver<T> {
    fn drop(&mut self) {
        let mut state = lock(&self.shared);
        state.receiver_alive = false;
        let pending = state.pending.take();
        drop(state);
        drop(pending);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn empty_receiver_has_no_snapshot() {
        let (_publisher, receiver) = channel::<u32>();

        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn published_snapshot_is_available() {
        let (publisher, receiver) = channel();

        assert!(publisher.publish("A"));
        assert_eq!(receiver.take_latest(), Some("A"));
    }

    #[test]
    fn newest_snapshot_replaces_all_obsolete_snapshots() {
        let (publisher, receiver) = channel();

        assert!(publisher.publish("A"));
        assert!(publisher.publish("B"));
        assert!(publisher.publish("C"));

        assert_eq!(receiver.take_latest(), Some("C"));
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn slot_can_be_reused_after_consumption() {
        let (publisher, receiver) = channel();

        assert!(publisher.publish(1));
        assert_eq!(receiver.take_latest(), Some(1));
        assert_eq!(receiver.take_latest(), None);

        assert!(publisher.publish(2));
        assert_eq!(receiver.take_latest(), Some(2));
    }

    #[test]
    fn rapid_publication_keeps_only_the_newest_value() {
        let (publisher, receiver) = channel();

        for value in 0..10_000 {
            assert!(publisher.publish(value));
        }

        assert_eq!(receiver.take_latest(), Some(9_999));
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn dropping_receiver_disconnects_publisher_without_panicking() {
        let (publisher, receiver) = channel();
        assert!(publisher.publish(1));

        drop(receiver);

        assert!(!publisher.publish(2));
    }

    #[test]
    fn dropping_publisher_preserves_the_last_pending_snapshot() {
        let (publisher, receiver) = channel();
        assert!(publisher.publish(7));

        drop(publisher);

        assert_eq!(receiver.take_latest(), Some(7));
        assert_eq!(receiver.take_latest(), None);
    }

    #[test]
    fn occupied_slot_does_not_block_worker_shutdown_or_join() {
        let (publisher, receiver) = channel();
        assert!(publisher.publish(1));
        let (published_tx, published_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();

        let worker = thread::spawn(move || {
            assert!(publisher.publish(2));
            published_tx.send(()).unwrap();
            stop_rx.recv().unwrap();
        });

        published_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        stop_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(receiver.take_latest(), Some(2));
    }
}
