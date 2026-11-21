// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Working on several of a run's items at once, on threads of its own, and taking what each gave
//! in the items' order, so that what a run does with them is the same however many threads work.

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Condvar, Mutex, MutexGuard, mpsc};
use std::thread;

/// Fewer threads started than were asked for, because the system would start no more.
#[derive(Debug)]
pub(super) struct Fewer {
    /// How many were asked for.
    pub(super) asked: usize,
    /// How many started; with none, the items were worked on the calling thread.
    pub(super) started: usize,
    /// The system's error, verbatim.
    pub(super) cause: io::Error,
}

/// Works on items `0..count` with `work`, on up to `jobs` threads at once, and hands what each
/// gave to `take`, on the calling thread, in the items' order.  With one job or one item, no
/// thread is started, and each item is worked on and taken before the next is started.
///
/// A thread starts an item only while fewer than twice `jobs` items are ahead of the next to be
/// taken, so that what waits to be taken stays in line with the number of threads, however long
/// one item takes.
///
/// Where the system will not start as many threads as asked for, the work goes on with those that
/// started, or on the calling thread with none, and the [`Fewer`] returned says so: what is taken
/// is the same either way.
///
/// # Errors
///
/// Returns the first error in the items' order, once every item before it has been taken; no
/// item after it is taken, and no more are started.
pub(super) fn in_order<T: Send, E: Send>(
    count: usize,
    jobs: NonZeroUsize,
    work: impl Fn(usize) -> Result<T, E> + Sync,
    mut take: impl FnMut(T),
) -> Result<Option<Fewer>, E> {
    let jobs = jobs.get().min(count);
    if jobs <= 1 {
        one_at_a_time(count, &work, &mut take)?;
        return Ok(None);
    }
    let shared = Shared {
        count,
        ahead: jobs * 2,
        state: Mutex::new(State {
            next: 0,
            taken: 0,
            stop: false,
        }),
        moved: Condvar::new(),
    };
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        // The calling thread stops the others if it panics too, since the scope waits for them
        // before passing the panic on, and one waiting for an item to be taken would never end.
        let _stopping = StopOnPanic(&shared);
        let mut fewer = None;
        for started in 0..jobs {
            let send = send.clone();
            let (shared, work) = (&shared, &work);
            let spawned = spawn(scope, started, move || {
                let _stopping = StopOnPanic(shared);
                while let Some(item) = shared.start() {
                    send.send((item, work(item)))
                        .expect("the calling thread receives until every thread has ended");
                }
            });
            if let Err(cause) = spawned {
                fewer = Some(Fewer {
                    asked: jobs,
                    started,
                    cause,
                });
                break;
            }
        }
        // Only the threads' copies are left, so the channel closes once every thread has ended.
        drop(send);
        if fewer.as_ref().is_some_and(|fewer| fewer.started == 0) {
            one_at_a_time(count, &work, &mut take)?;
            return Ok(fewer);
        }
        let mut waiting = BTreeMap::new();
        let mut next = 0;
        let result = loop {
            if next == count {
                break Ok(fewer);
            }
            match waiting.remove(&next) {
                Some(Ok(value)) => {
                    take(value);
                    next += 1;
                    shared.lock().taken = next;
                    shared.moved.notify_all();
                }
                Some(Err(err)) => break Err(err),
                None => {
                    let (item, result) = receive
                        .recv()
                        .expect("only a thread that panicked leaves an item unsent");
                    waiting.insert(item, result);
                }
            }
        };
        shared.stop();
        result
    })
}

/// Works on items `0..count` and takes each, one after another, on the calling thread.
fn one_at_a_time<T, E>(
    count: usize,
    work: &impl Fn(usize) -> Result<T, E>,
    take: &mut impl FnMut(T),
) -> Result<(), E> {
    for item in 0..count {
        take(work(item)?);
    }
    Ok(())
}

/// Starts the thread `index`, counted from 0, in `scope`, running `run`.
///
/// # Errors
///
/// Returns the system's error, verbatim, when it will not start one.
fn spawn<'scope>(
    scope: &'scope thread::Scope<'scope, '_>,
    index: usize,
    run: impl FnOnce() + Send + 'scope,
) -> io::Result<()> {
    #[cfg(test)]
    if REFUSED_FROM
        .with(std::cell::Cell::get)
        .is_some_and(|from| index >= from)
    {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "refused by a test",
        ));
    }
    #[cfg(not(test))]
    let _ = index;
    thread::Builder::new().spawn_scoped(scope, run).map(drop)
}

#[cfg(test)]
thread_local! {
    /// The first thread a test has the system refuse to start, on the thread the test runs on.
    static REFUSED_FROM: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// Calls `run` with the system refusing to start thread `from` and every one after it, counted
/// from 0, for any run on this thread.
#[cfg(test)]
pub(super) fn with_threads_refused_from<R>(from: usize, run: impl FnOnce() -> R) -> R {
    /// Lets the system start threads again however `run` ends, a failed assertion included.
    struct Allowed;
    impl Drop for Allowed {
        fn drop(&mut self) {
            REFUSED_FROM.with(|refused| refused.set(None));
        }
    }
    REFUSED_FROM.with(|refused| refused.set(Some(from)));
    let _allowed = Allowed;
    run()
}

/// What the threads working on items share.
struct Shared {
    /// How many items there are.
    count: usize,
    /// How far past the next item to be taken a thread may start one.
    ahead: usize,
    /// Where the work is, behind one lock, so that whether a thread may start an item is decided
    /// on one consistent view of it.
    state: Mutex<State>,
    /// Told when an item is taken, or the threads are to stop.
    moved: Condvar,
}

/// Where the work on the items is.
struct State {
    /// The next item to start.
    next: usize,
    /// How many items have been taken, in order.
    taken: usize,
    /// Set once nothing more is to be started: the items are all taken, an error ends them, or a
    /// thread panicked.
    stop: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("no thread panics holding it")
    }

    /// The next item for a thread to work on, once it is fewer than `ahead` items past the next
    /// to be taken, or `None` once there are no more or the threads are to stop.
    fn start(&self) -> Option<usize> {
        let mut state = self.lock();
        // The item to be taken next has been started whenever a thread waits here, since it is
        // less than `ahead` past itself, so some thread is always working while others wait.
        while !state.stop && state.next < self.count && state.next >= state.taken + self.ahead {
            state = self.moved.wait(state).expect("no thread panics holding it");
        }
        if state.stop || state.next >= self.count {
            return None;
        }
        state.next += 1;
        Some(state.next - 1)
    }

    /// Tells every thread to start nothing more.
    fn stop(&self) {
        self.lock().stop = true;
        self.moved.notify_all();
    }
}

/// Stops every thread if the one holding it panics, so that none waits for an item that will
/// never be taken, and the panic reaches the calling thread once they have ended.
struct StopOnPanic<'a>(&'a Shared);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn jobs(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("not zero")
    }

    #[test]
    fn items_are_taken_in_order_however_long_each_takes() {
        for n in [1, 2, 3, 8] {
            let mut taken = Vec::new();
            let result: Result<_, ()> = in_order(
                40,
                jobs(n),
                |item| {
                    // Later items finish first.
                    thread::sleep(Duration::from_micros(
                        u64::try_from((40 - item) * 50).unwrap(),
                    ));
                    Ok(item)
                },
                |item| taken.push(item),
            );
            assert!(matches!(result, Ok(None)), "{result:?}");
            assert_eq!(taken, (0..40).collect::<Vec<_>>(), "with {n} jobs");
        }
    }

    #[test]
    fn the_first_error_in_order_wins_and_nothing_after_it_is_taken() {
        for n in [1, 4] {
            let mut taken = Vec::new();
            let result = in_order(
                100,
                jobs(n),
                |item| match item {
                    // With several jobs the later error is found first, and every thread runs
                    // as far ahead as it may and waits, until the earlier error stops them.
                    3 => {
                        thread::sleep(Duration::from_millis(50));
                        Err(item)
                    }
                    5 => Err(item),
                    _ => Ok(item),
                },
                |item| taken.push(item),
            );
            assert!(matches!(result, Err(3)), "with {n} jobs: {result:?}");
            assert_eq!(taken, [0, 1, 2], "with {n} jobs");
        }
    }

    #[test]
    fn several_jobs_work_on_several_items_at_once() {
        // Each item waits for the others to start: one thread at a time would never get past
        // the first.
        let met = Barrier::new(4);
        let result: Result<_, ()> = in_order(
            4,
            jobs(4),
            |_| {
                met.wait();
                Ok(())
            },
            |()| {},
        );
        assert!(matches!(result, Ok(None)), "{result:?}");
    }

    #[test]
    fn no_thread_starts_an_item_far_ahead_of_the_next_to_be_taken() {
        let first_taken = AtomicBool::new(false);
        let started_early = Mutex::new(Vec::new());
        let ahead_done = AtomicUsize::new(0);
        let result: Result<_, ()> = in_order(
            50,
            jobs(2),
            |item| {
                if !first_taken.load(Ordering::SeqCst) {
                    started_early.lock().unwrap().push(item);
                }
                if item == 0 {
                    // Not done until the other thread has done every item it may ahead of this
                    // one, and has had a moment to ask for the next, which it must wait for.
                    let deadline = Instant::now() + Duration::from_secs(10);
                    while ahead_done.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(1));
                    }
                    thread::sleep(Duration::from_millis(20));
                } else {
                    ahead_done.fetch_add(1, Ordering::SeqCst);
                }
                Ok(item)
            },
            |item| {
                if item == 0 {
                    first_taken.store(true, Ordering::SeqCst);
                }
            },
        );
        assert!(matches!(result, Ok(None)), "{result:?}");
        let mut started_early = started_early.into_inner().unwrap();
        started_early.sort_unstable();
        // Two jobs may be four items ahead of the next to be taken: item 0 itself, and 1 to 3.
        assert_eq!(started_early, [0, 1, 2, 3]);
    }

    #[test]
    fn with_fewer_threads_than_asked_for_the_work_goes_on_and_says_so() {
        for (from, started) in [(0, 0), (1, 1), (3, 3)] {
            let mut taken = Vec::new();
            let result: Result<_, ()> = with_threads_refused_from(from, || {
                in_order(30, jobs(4), Ok, |item| taken.push(item))
            });
            let fewer = result
                .unwrap()
                .expect("fewer threads started than asked for");
            assert_eq!((fewer.asked, fewer.started), (4, started));
            assert_eq!(fewer.cause.to_string(), "refused by a test");
            assert_eq!(taken, (0..30).collect::<Vec<_>>(), "with {started} started");
        }
        // With none, the calling thread stops at the first error as it does with one job.
        let result = with_threads_refused_from(0, || {
            in_order(
                30,
                jobs(4),
                |item| match item {
                    7 => Err(item),
                    _ => Ok(item),
                },
                |_| {},
            )
        });
        assert!(matches!(result, Err(7)), "{result:?}");
    }

    #[test]
    fn a_panic_on_a_thread_reaches_the_caller_rather_than_hanging() {
        let first_taken = AtomicBool::new(false);
        let panicked = std::panic::catch_unwind(|| {
            let _: Result<_, ()> = in_order(
                20,
                jobs(4),
                |item| {
                    // Item 1 fails once item 0 is taken, with the threads ahead of it waiting.
                    if item == 1 {
                        let deadline = std::time::Instant::now() + Duration::from_secs(10);
                        while !first_taken.load(Ordering::SeqCst)
                            && std::time::Instant::now() < deadline
                        {
                            thread::sleep(Duration::from_millis(1));
                        }
                        panic!("item 1 fails");
                    }
                    Ok(())
                },
                |()| first_taken.store(true, Ordering::SeqCst),
            );
        });
        assert!(panicked.is_err());
        assert!(first_taken.into_inner(), "item 0 was never taken");
    }
}
