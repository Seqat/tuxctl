//! Stop/pause/refresh signalling and the sampling period shared by the
//! background collectors.

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

struct CollectorControlState {
    pending_refresh_generation: Option<RefreshGeneration>,
    stop: bool,
    paused: bool,
    /// Time between the end of one collection and the start of the next.
    period: Duration,
}

pub(super) struct CollectorControl {
    state: Mutex<CollectorControlState>,
    wake: Condvar,
}

/// For callers that only use the stop/refresh signalling.
impl Default for CollectorControl {
    fn default() -> Self {
        Self::new(Duration::from_secs(1), false)
    }
}

impl CollectorControl {
    pub(super) fn new(period: Duration, paused: bool) -> Self {
        Self {
            state: Mutex::new(CollectorControlState {
                pending_refresh_generation: None,
                stop: false,
                paused,
                period,
            }),
            wake: Condvar::new(),
        }
    }

    /// Changes the sampling period. A waiting worker wakes and measures the new
    /// period from the end of its last collection: it collects at once if that
    /// much time has already passed, otherwise it waits for the remainder.
    pub(super) fn set_period(&self, period: Duration) {
        let mut state = lock(&self.state);
        if state.period == period {
            return;
        }
        state.period = period;
        drop(state);
        self.wake.notify_one();
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
        self.wait_until(|_| deadline)
    }

    /// Like [`Self::wait`] for one sampling period, except that a period changed
    /// during the wait applies at once (see [`Self::set_period`]).
    pub(super) fn wait_period(&self) -> CollectorWake {
        let started = Instant::now();
        self.wait_until(|state| started + state.period)
    }

    fn wait_until(&self, deadline: impl Fn(&CollectorControlState) -> Instant) -> CollectorWake {
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
            let remaining = deadline(&state).saturating_duration_since(Instant::now());
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

/// Collects and publishes once per sampling period of `control` until stopped
/// or until the receiving side is gone (`publish` returns `false`).
pub(super) fn run_periodic<T>(
    control: &CollectorControl,
    mut collect: impl FnMut() -> T,
    mut publish: impl FnMut(T) -> bool,
) {
    loop {
        if !publish(collect()) {
            break;
        }
        if control.wait_period() == CollectorWake::Stop {
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
        let control = Arc::new(CollectorControl::new(Duration::from_secs(1), true));
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_millis(10)));

        thread::sleep(Duration::from_millis(100));
        assert!(!worker.is_finished(), "a paused worker must not time out");
        control.set_paused(false);

        assert_eq!(worker.join().unwrap(), CollectorWake::Timeout);
    }

    #[test]
    fn refresh_requested_while_paused_is_served_once_on_resume() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(1), true));
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
        let control = Arc::new(CollectorControl::new(Duration::from_secs(1), true));
        let worker_control = Arc::clone(&control);
        let worker = thread::spawn(move || worker_control.wait(Duration::from_secs(3600)));

        control.stop();

        assert_eq!(worker.join().unwrap(), CollectorWake::Stop);
    }

    /// Runs `wait_period` on a worker; returns its outcome and how long it took.
    fn timed_period_wait(
        control: &Arc<CollectorControl>,
    ) -> thread::JoinHandle<(CollectorWake, Duration)> {
        let worker_control = Arc::clone(control);
        thread::spawn(move || {
            let started = Instant::now();
            (worker_control.wait_period(), started.elapsed())
        })
    }

    #[test]
    fn a_shorter_period_applies_from_the_start_of_the_wait() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), false));
        let worker = timed_period_wait(&control);

        thread::sleep(Duration::from_millis(50));
        control.set_period(Duration::from_millis(300));

        let (wake, waited) = worker.join().unwrap();
        assert_eq!(wake, CollectorWake::Timeout);
        assert!(waited >= Duration::from_millis(300), "{waited:?}");
        assert!(waited < Duration::from_secs(3), "{waited:?}");
    }

    #[test]
    fn a_period_already_elapsed_collects_at_once() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), false));
        let worker = timed_period_wait(&control);

        thread::sleep(Duration::from_millis(150));
        let changed = Instant::now();
        control.set_period(Duration::from_millis(50));

        let (wake, _) = worker.join().unwrap();
        assert_eq!(wake, CollectorWake::Timeout);
        assert!(changed.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_longer_period_extends_the_wait() {
        let control = Arc::new(CollectorControl::new(Duration::from_millis(100), false));
        let worker = timed_period_wait(&control);

        thread::sleep(Duration::from_millis(20));
        control.set_period(Duration::from_millis(400));

        let (wake, waited) = worker.join().unwrap();
        assert_eq!(wake, CollectorWake::Timeout);
        assert!(waited >= Duration::from_millis(400), "{waited:?}");
    }

    #[test]
    fn a_period_set_while_paused_applies_on_resume() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), true));
        let worker = timed_period_wait(&control);

        control.set_period(Duration::from_millis(10));
        thread::sleep(Duration::from_millis(100));
        assert!(!worker.is_finished(), "a paused worker must not collect");
        control.set_paused(false);

        let (wake, waited) = worker.join().unwrap();
        assert_eq!(wake, CollectorWake::Timeout);
        assert!(waited < Duration::from_secs(3), "{waited:?}");
    }

    #[test]
    fn stop_wins_over_a_period_change() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), false));
        let worker = timed_period_wait(&control);

        control.stop();
        control.set_period(Duration::ZERO);

        assert_eq!(worker.join().unwrap().0, CollectorWake::Stop);
    }

    #[test]
    fn repeated_period_changes_do_not_force_extra_collections() {
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), false));
        let worker_control = Arc::clone(&control);
        let collections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_collections = Arc::clone(&collections);
        let worker = thread::spawn(move || {
            run_periodic(
                &worker_control,
                || {
                    worker_collections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
                |()| true,
            );
        });

        thread::sleep(Duration::from_millis(50));
        for step in 0..100 {
            // Key repeat over presets that are all longer than the time waited.
            control.set_period(Duration::from_secs(if step % 2 == 0 { 60 } else { 30 }));
        }
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            collections.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only the initial collection"
        );

        control.stop();
        worker.join().unwrap();
    }

    #[test]
    fn periodic_worker_publishes_until_stopped() {
        let control = Arc::new(CollectorControl::new(Duration::from_millis(5), false));
        let worker_control = Arc::clone(&control);
        let (published_tx, published_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut sample = 0;
            run_periodic(
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
        let control = Arc::new(CollectorControl::new(Duration::from_secs(3600), false));
        let worker_control = Arc::clone(&control);
        let (published_tx, published_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            run_periodic(&worker_control, || (), |()| published_tx.send(()).is_ok());
        });
        published_rx.recv().unwrap();

        let started = Instant::now();
        control.stop();
        worker.join().unwrap();

        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn dropped_receiver_ends_the_periodic_worker() {
        let control = CollectorControl::new(Duration::from_secs(3600), false);
        let mut collections = 0;

        run_periodic(&control, || collections += 1, |()| false);

        assert_eq!(collections, 1);
    }
}
