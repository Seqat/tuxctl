//! Stop/refresh signalling shared by the background collectors.

use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Monotonic id of a user-requested refresh.
pub(super) type RefreshGeneration = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CollectorWake {
    Refresh(RefreshGeneration),
    Stop,
    Timeout,
}

#[derive(Default)]
struct CollectorControlState {
    pending_refresh_generation: Option<RefreshGeneration>,
    stop: bool,
    paused: bool,
}

#[derive(Default)]
pub(super) struct CollectorControl {
    state: Mutex<CollectorControlState>,
    wake: Condvar,
}

impl CollectorControl {
    pub(super) fn new_paused(paused: bool) -> Self {
        let control = Self::default();
        lock(&control.state).paused = paused;
        control
    }

    /// While paused, a waiting worker does not collect; a refresh requested
    /// meanwhile is kept and served on resume.
    pub(super) fn set_paused(&self, paused: bool) {
        let mut state = lock(&self.state);
        if state.paused == paused {
            return;
        }
        state.paused = paused;
        drop(state);
        self.wake.notify_one();
    }

    pub(super) fn request_refresh(&self, generation: RefreshGeneration) {
        let mut state = lock(&self.state);
        if state.stop {
            return;
        }
        state.pending_refresh_generation = Some(
            state
                .pending_refresh_generation
                .map_or(generation, |pending| pending.max(generation)),
        );
        drop(state);
        self.wake.notify_one();
    }

    pub(super) fn stop(&self) {
        let mut state = lock(&self.state);
        state.stop = true;
        drop(state);
        self.wake.notify_one();
    }

    /// Sleeps for up to `timeout`, returning early with `true` once a stop is requested.
    pub(super) fn stopped_within(&self, timeout: Duration) -> bool {
        let state = lock(&self.state);
        let (state, _) = self
            .wake
            .wait_timeout_while(state, timeout, |state| !state.stop)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.stop
    }

    /// Waits until a stop, a refresh request, or `timeout` after the call.
    /// Time spent paused counts toward `timeout`, so resuming after a long
    /// pause collects promptly.
    pub(super) fn wait(&self, timeout: Duration) -> CollectorWake {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            if state.stop {
                return CollectorWake::Stop;
            }
            if state.paused {
                state = self
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                continue;
            }
            if let Some(generation) = state.pending_refresh_generation.take() {
                return CollectorWake::Refresh(generation);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return CollectorWake::Timeout;
            }
            state = self
                .wake
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

/// Collects and publishes once per `refresh_rate` until stopped or until the
/// receiving side is gone (`publish` returns `false`).
pub(super) fn run_periodic<T>(
    refresh_rate: Duration,
    control: &CollectorControl,
    mut collect: impl FnMut() -> T,
    mut publish: impl FnMut(T) -> bool,
) {
    loop {
        if !publish(collect()) {
            break;
        }
        if control.wait(refresh_rate) == CollectorWake::Stop {
            break;
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{mpsc, Arc},
        thread,
        time::Instant,
    };

    use super::*;

    #[test]
    fn repeated_refresh_requests_coalesce_to_one_pending_wake() {
        let control = CollectorControl::default();

        for generation in 1..=10_000 {
            control.request_refresh(generation);
        }

        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Refresh(10_000));
        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Timeout);
    }

    #[test]
    fn stop_takes_priority_over_a_pending_refresh() {
        let control = CollectorControl::default();
        control.request_refresh(1);

        control.stop();

        assert_eq!(control.wait(Duration::ZERO), CollectorWake::Stop);
    }

    #[test]
    fn stop_wakes_and_joins_a_waiting_worker() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_secs(60)));

        control.stop();

        assert_eq!(worker.join().unwrap(), CollectorWake::Stop);
    }

    #[test]
    fn paused_wait_ignores_timeouts_until_resumed() {
        let control = Arc::new(CollectorControl::new_paused(true));
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_millis(10)));

        thread::sleep(Duration::from_millis(100));
        assert!(!worker.is_finished(), "a paused worker must not time out");
        control.set_paused(false);

        assert_eq!(worker.join().unwrap(), CollectorWake::Timeout);
    }

    #[test]
    fn refresh_requested_while_paused_is_served_once_on_resume() {
        let control = Arc::new(CollectorControl::new_paused(true));
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || {
            (
                worker_control.wait(Duration::from_secs(3600)),
                worker_control.wait(Duration::ZERO),
            )
        });

        control.request_refresh(3);
        thread::sleep(Duration::from_millis(50));
        assert!(!worker.is_finished());
        control.set_paused(false);

        assert_eq!(
            worker.join().unwrap(),
            (CollectorWake::Refresh(3), CollectorWake::Timeout)
        );
    }

    #[test]
    fn stop_wakes_a_paused_worker() {
        let control = Arc::new(CollectorControl::new_paused(true));
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_secs(3600)));

        control.stop();

        assert_eq!(worker.join().unwrap(), CollectorWake::Stop);
    }

    #[test]
    fn periodic_worker_publishes_until_stopped() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let (published_tx, published_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut sample = 0;
            run_periodic(
                Duration::from_millis(5),
                &worker_control,
                || {
                    sample += 1;
                    sample
                },
                |sample| published_tx.send(sample).is_ok(),
            );
        });

        assert_eq!(published_rx.recv().unwrap(), 1);
        assert_eq!(published_rx.recv().unwrap(), 2);
        control.stop();
        worker.join().unwrap();
    }

    #[test]
    fn stop_interrupts_a_long_periodic_wait() {
        let control = Arc::new(CollectorControl::default());
        let worker_control = Arc::clone(&control);
        let (published_tx, published_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_periodic(
                Duration::from_secs(3600),
                &worker_control,
                || (),
                |()| published_tx.send(()).is_ok(),
            );
        });
        published_rx.recv().unwrap();

        let started = Instant::now();
        control.stop();
        worker.join().unwrap();

        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn dropped_receiver_ends_the_periodic_worker() {
        let control = CollectorControl::default();
        let mut collections = 0;

        run_periodic(
            Duration::from_secs(3600),
            &control,
            || collections += 1,
            |()| false,
        );

        assert_eq!(collections, 1);
    }
}
