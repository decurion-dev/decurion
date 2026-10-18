//! How many pieces of work a runner runs at once, and the loop that keeps
//! that many going. It owns which pieces are still busy (running, or
//! cleaning up after a run) so the hub never hands one back while an earlier
//! attempt is still on this computer.
//!
//! Rules that must stay true:
//! - At most one claim is asked at a time, and it is never dropped once
//!   asked: the hub has already marked that work running, so losing the
//!   answer would leave it stuck. A run finishing meanwhile does not cancel
//!   it.
//! - Runs and the pending claim are polled together, so runs keep going
//!   while a claim waits or backs off.
//! - A piece leaves the busy list only when its whole run future has ended.
//! - When told to drain (the hub runs other rules), no more claims are
//!   asked, and the loop ends only once every run has ended.

use decurion_proto::{Place, MAX_RUNS};
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;

/// Pieces of work at once on the PC unless `DECURION_MAX_RUNS` says
/// otherwise. A cloud worker is usually a small server, so it runs one.
pub const PC_DEFAULT: usize = 3;

/// How many pieces of work run at once: `DECURION_MAX_RUNS` when set (1 to
/// [`MAX_RUNS`]), otherwise [`PC_DEFAULT`] on the PC and 1 on a cloud worker.
pub fn max_runs(place: Place, setting: Option<&str>) -> Result<usize, String> {
    match setting.map(str::trim) {
        None | Some("") => Ok(match place {
            Place::Pc => PC_DEFAULT,
            Place::Cloud => 1,
        }),
        Some(s) => match s.parse::<usize>() {
            Ok(n) if (1..=MAX_RUNS).contains(&n) => Ok(n),
            _ => Err(format!(
                "DECURION_MAX_RUNS must be a whole number from 1 to {MAX_RUNS}, not \"{s}\""
            )),
        },
    }
}

/// What asking the hub for work came to.
pub enum Next<C> {
    /// Work to run, and its thread id.
    Work(i64, C),
    /// Nothing to run (the claim already waited before saying so).
    Idle,
    /// Stop asking: finish what is running, then end.
    Drain,
}

/// Keeps up to `max` runs going. `claim` gets the busy thread ids and asks
/// the hub for one more piece; it should wait a little itself before
/// returning [`Next::Idle`]. `run` runs one piece to the very end, cleanup
/// included. Returns once drained and every run has ended.
pub async fn dispatch<C, CF, RF>(
    max: usize,
    mut claim: impl FnMut(Vec<i64>) -> CF,
    mut run: impl FnMut(C) -> RF,
) where
    CF: Future<Output = Next<C>>,
    RF: Future<Output = ()>,
{
    let max = max.max(1);
    let mut busy: BTreeSet<i64> = BTreeSet::new();
    let mut runs = FuturesUnordered::new();
    let mut pending: Option<Pin<Box<CF>>> = None;
    let mut draining = false;
    loop {
        if !draining && pending.is_none() && busy.len() < max {
            pending = Some(Box::pin(claim(busy.iter().copied().collect())));
        }
        if draining && runs.is_empty() {
            return;
        }
        tokio::select! {
            Some(id) = runs.next(), if !runs.is_empty() => {
                busy.remove(&id);
            }
            next = async { pending.as_mut().expect("guarded by is_some").as_mut().await },
                if pending.is_some() =>
            {
                pending = None;
                match next {
                    Next::Work(id, c) => {
                        busy.insert(id);
                        let f = run(c);
                        runs.push(async move {
                            f.await;
                            id
                        });
                    }
                    Next::Idle => {}
                    Next::Drain => draining = true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    use tokio::sync::oneshot;

    #[test]
    fn three_at_once_on_the_pc_and_one_on_a_cloud_worker() {
        assert_eq!(max_runs(Place::Pc, None), Ok(3));
        assert_eq!(max_runs(Place::Cloud, None), Ok(1));
        assert_eq!(max_runs(Place::Pc, Some("")), Ok(3));
        assert_eq!(max_runs(Place::Cloud, Some(" 2 ")), Ok(2));
        assert_eq!(max_runs(Place::Pc, Some("8")), Ok(8));
        for bad in ["0", "9", "-1", "three", "2.5"] {
            assert!(max_runs(Place::Pc, Some(bad)).is_err(), "{bad}");
        }
    }

    /// Lets the dispatcher run until it has nothing left to do right now.
    async fn settle() {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    /// A fake hub with `n` queued pieces, then Drain. Records each claim's
    /// busy list; each run waits until the test lets it finish.
    struct Rig {
        asks: Rc<RefCell<Vec<Vec<i64>>>>,
        started: Rc<RefCell<Vec<i64>>>,
        finish: Rc<RefCell<HashMap<i64, oneshot::Sender<()>>>>,
    }

    impl Rig {
        fn new() -> Self {
            Rig {
                asks: Rc::default(),
                started: Rc::default(),
                finish: Rc::default(),
            }
        }
        fn end(&self, id: i64) {
            let tx = self.finish.borrow_mut().remove(&id).expect("running");
            tx.send(()).expect("run is waiting");
        }
        fn asks(&self) -> usize {
            self.asks.borrow().len()
        }
        fn started(&self) -> Vec<i64> {
            self.started.borrow().clone()
        }
    }

    #[tokio::test]
    async fn three_run_at_once_and_a_fourth_waits_for_a_free_place() {
        let rig = Rig::new();
        let asks = rig.asks.clone();
        let started = rig.started.clone();
        let finish = rig.finish.clone();
        let next_id = Rc::new(RefCell::new(0i64));
        let d = dispatch(
            3,
            move |busy| {
                asks.borrow_mut().push(busy);
                let mut n = next_id.borrow_mut();
                *n += 1;
                let id = *n;
                async move {
                    if id <= 5 {
                        Next::Work(id, id)
                    } else {
                        Next::Drain
                    }
                }
            },
            move |id: i64| {
                let (tx, rx) = oneshot::channel();
                finish.borrow_mut().insert(id, tx);
                started.borrow_mut().push(id);
                async move {
                    let _ = rx.await;
                }
            },
        );
        let test = async {
            settle().await;
            assert_eq!(rig.started(), vec![1, 2, 3]);
            assert_eq!(rig.asks(), 3, "no fourth claim while three run");
            rig.end(2);
            settle().await;
            assert_eq!(rig.started(), vec![1, 2, 3, 4]);
            assert_eq!(
                rig.asks.borrow()[3],
                vec![1, 3],
                "busy lists what still runs"
            );
            rig.end(1);
            settle().await;
            assert_eq!(rig.started(), vec![1, 2, 3, 4, 5]);
            rig.end(3);
            settle().await;
            // Asked again (and told to drain); 4 and 5 still run.
            assert_eq!(rig.asks(), 6);
            rig.end(4);
            rig.end(5);
        };
        tokio::join!(d, test);
        assert_eq!(rig.asks(), 6, "nothing asked after draining");
    }

    /// A claim that is slow to answer is not dropped when a run ends
    /// meanwhile: the hub has already marked that work running.
    #[tokio::test]
    async fn a_slow_claim_is_not_dropped_when_a_run_ends() {
        let rig = Rig::new();
        let asks = rig.asks.clone();
        let started = rig.started.clone();
        let finish = rig.finish.clone();
        let (answer_tx, answer_rx) = oneshot::channel::<()>();
        let answer_rx = Rc::new(RefCell::new(Some(answer_rx)));
        let d = dispatch(
            3,
            move |busy| {
                asks.borrow_mut().push(busy);
                let n = asks.borrow().len();
                let wait = if n == 2 {
                    answer_rx.borrow_mut().take()
                } else {
                    None
                };
                async move {
                    match n {
                        1 => Next::Work(1, 1),
                        2 => {
                            if let Some(w) = wait {
                                let _ = w.await;
                            }
                            Next::Work(2, 2)
                        }
                        _ => Next::Drain,
                    }
                }
            },
            move |id: i64| {
                let (tx, rx) = oneshot::channel();
                finish.borrow_mut().insert(id, tx);
                started.borrow_mut().push(id);
                async move {
                    let _ = rx.await;
                }
            },
        );
        let test = async {
            settle().await;
            assert_eq!(rig.started(), vec![1]);
            assert_eq!(rig.asks(), 2, "second claim is waiting on the hub");
            // Run 1 ends while the claim is still out; it keeps going.
            rig.end(1);
            settle().await;
            assert_eq!(rig.asks(), 2, "the waiting claim was not asked again");
            answer_tx.send(()).expect("claim waits");
            settle().await;
            assert_eq!(rig.started(), vec![1, 2]);
            rig.end(2);
        };
        tokio::join!(d, test);
    }

    /// Told to drain while work runs, the loop asks for nothing more and
    /// ends only when that work has ended, so it is never cut off midway.
    #[tokio::test]
    async fn draining_lets_running_work_finish_first() {
        let rig = Rig::new();
        let asks = rig.asks.clone();
        let started = rig.started.clone();
        let finish = rig.finish.clone();
        let done = Rc::new(RefCell::new(false));
        let d = {
            let done = done.clone();
            async move {
                dispatch(
                    3,
                    move |busy| {
                        asks.borrow_mut().push(busy);
                        let n = asks.borrow().len();
                        async move {
                            if n == 1 {
                                Next::Work(1, 1)
                            } else {
                                Next::Drain
                            }
                        }
                    },
                    move |id: i64| {
                        let (tx, rx) = oneshot::channel();
                        finish.borrow_mut().insert(id, tx);
                        started.borrow_mut().push(id);
                        async move {
                            let _ = rx.await;
                        }
                    },
                )
                .await;
                *done.borrow_mut() = true;
            }
        };
        let test = async {
            settle().await;
            assert_eq!(rig.asks(), 2);
            assert!(!*done.borrow(), "still waiting on the running work");
            rig.end(1);
            settle().await;
            assert!(*done.borrow());
            assert_eq!(rig.asks(), 2);
        };
        tokio::join!(d, test);
    }
}
