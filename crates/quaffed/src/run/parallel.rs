// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Gavin Lucas

//! Working on several of a run's items at once, on threads of its own, and taking what each gave
//! in the items' order, so that what a run does with them is the same however many threads work.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, mpsc};
use std::thread;

/// Works on items `0..count` with `work`, on up to `jobs` threads at once, and hands what each
/// gave to `take`, on the calling thread, in the items' order.  With one job or one item, no
/// thread is started, and each item is worked on and taken before the next is started.
///
/// A thread starts an item only while fewer than twice `jobs` items are ahead of the next to be
/// taken, so that what waits to be taken stays in line with the number of threads, however long
/// one item takes.
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
) -> Result<(), E> {
    let jobs = jobs.get().min(count);
    if jobs <= 1 {
        for item in 0..count {
            take(work(item)?);
        }
        return Ok(());
    }
    let ahead = jobs * 2;
    let shared = Shared {
        next: AtomicUsize::new(0),
        taken: Mutex::new(0),
        moved: Condvar::new(),
        stop: AtomicBool::new(false),
    };
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..jobs {
            let send = send.clone();
            let (shared, work) = (&shared, &work);
            scope.spawn(move || {
                let _stopping = StopOnPanic(shared);
                while let Some(item) = shared.start(count, ahead) {
                    if send.send((item, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
        // Only the threads' copies are left, so the channel closes once every thread has ended.
        drop(send);
        let mut waiting = BTreeMap::new();
        let mut next = 0;
        let result = loop {
            if next == count {
                break Ok(());
            }
            match waiting.remove(&next) {
                Some(Ok(value)) => {
                    take(value);
                    next += 1;
                    *shared.taken.lock().expect("no thread panics holding it") = next;
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

/// What the threads working on items share.
struct Shared {
    /// The next item to start.
    next: AtomicUsize,
    /// How many items have been taken, in order.
    taken: Mutex<usize>,
    /// Told when `taken` moves, or the threads are to stop.
    moved: Condvar,
    /// Set once nothing more is to be started: the items are all taken, an error ends them, or a
    /// thread panicked.
    stop: AtomicBool,
}

impl Shared {
    /// The next item for a thread to work on, once it is fewer than `ahead` items past the next
    /// to be taken, or `None` once there are no more or the threads are to stop.
    fn start(&self, count: usize, ahead: usize) -> Option<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return None;
        }
        let item = self.next.fetch_add(1, Ordering::Relaxed);
        if item >= count {
            return None;
        }
        let mut taken = self.taken.lock().expect("no thread panics holding it");
        // The item being waited on to be taken next is never itself waiting here, since it is
        // less than `ahead` past itself, so some thread is always working.
        while item >= *taken + ahead && !self.stop.load(Ordering::Relaxed) {
            taken = self.moved.wait(taken).expect("no thread panics holding it");
        }
        (!self.stop.load(Ordering::Relaxed)).then_some(item)
    }

    /// Tells every thread to start nothing more.
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Taken, so that no thread can be between checking `stop` and waiting when told.
        let _taken = self.taken.lock().expect("no thread panics holding it");
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
    use std::time::Duration;

    fn jobs(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).expect("not zero")
    }

    #[test]
    fn items_are_taken_in_order_however_long_each_takes() {
        for n in [1, 2, 3, 8] {
            let mut taken = Vec::new();
            let result: Result<(), ()> = in_order(
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
            assert_eq!(result, Ok(()));
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
                    // The later error is found first.
                    30 => {
                        thread::sleep(Duration::from_millis(50));
                        Err(item)
                    }
                    70 => Err(item),
                    _ => Ok(item),
                },
                |item| taken.push(item),
            );
            assert_eq!(result, Err(30), "with {n} jobs");
            assert_eq!(taken, (0..30).collect::<Vec<_>>(), "with {n} jobs");
        }
    }

    #[test]
    fn several_jobs_work_on_several_items_at_once() {
        // Each item waits for the others to start: one thread at a time would never get past
        // the first.
        let met = Barrier::new(4);
        let result: Result<(), ()> = in_order(
            4,
            jobs(4),
            |_| {
                met.wait();
                Ok(())
            },
            |()| {},
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn no_thread_starts_an_item_far_ahead_of_the_next_to_be_taken() {
        let first_taken = AtomicBool::new(false);
        let started_early = Mutex::new(Vec::new());
        let result: Result<(), ()> = in_order(
            50,
            jobs(2),
            |item| {
                if item == 0 {
                    // Long enough for the other thread to run as far ahead as it may.
                    thread::sleep(Duration::from_millis(100));
                } else if !first_taken.load(Ordering::SeqCst) {
                    started_early.lock().unwrap().push(item);
                }
                Ok(item)
            },
            |item| {
                if item == 0 {
                    first_taken.store(true, Ordering::SeqCst);
                }
            },
        );
        assert_eq!(result, Ok(()));
        let started_early = started_early.into_inner().unwrap();
        // Two jobs may be four ahead of item 0: items 1 to 3, and item 0 itself.
        assert_eq!(started_early, [1, 2, 3]);
    }

    #[test]
    fn a_panic_on_a_thread_reaches_the_caller_rather_than_hanging() {
        let panicked = std::panic::catch_unwind(|| {
            let _: Result<(), ()> = in_order(
                20,
                jobs(4),
                |item| {
                    assert_ne!(item, 0, "item 0 fails");
                    Ok(())
                },
                |()| {},
            );
        });
        assert!(panicked.is_err());
    }
}
