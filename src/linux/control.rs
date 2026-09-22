//! Stop/refresh signalling shared by the background collectors.

use std::{
    sync::{Condvar, Mutex, MutexGuard},
    time::Duration,
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
}

#[derive(Default)]
pub(super) struct CollectorControl {
    state: Mutex<CollectorControlState>,
    wake: Condvar,
}

impl CollectorControl {
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

    pub(super) fn wait(&self, timeout: Duration) -> CollectorWake {
        let mut state = lock(&self.state);
        if state.pending_refresh_generation.is_none() && !state.stop {
            let (next_state, _) = self
                .wake
                .wait_timeout_while(state, timeout, |state| {
                    state.pending_refresh_generation.is_none() && !state.stop
                })
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
        }

        if state.stop {
            CollectorWake::Stop
        } else if let Some(generation) = state.pending_refresh_generation.take() {
            CollectorWake::Refresh(generation)
        } else {
            CollectorWake::Timeout
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
