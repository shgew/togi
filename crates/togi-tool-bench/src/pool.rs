use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

/// Calls `job` for each index of `order`, up to `workers` at a time and in that order. The first error stops
/// further starts; the errors that did occur come back with the index that raised each, in index order, so the
/// caller's choice among them does not depend on which worker finished first.
pub(crate) fn run<E: Send>(
    workers: usize,
    order: &[usize],
    job: impl Fn(usize) -> Result<(), E> + Sync,
) -> Vec<(usize, E)> {
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let errors = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..workers.min(order.len()) {
            scope.spawn(|| {
                while !stopped.load(Ordering::SeqCst) {
                    let Some(&index) = order.get(next.fetch_add(1, Ordering::SeqCst)) else {
                        return;
                    };
                    if let Err(error) = job(index) {
                        stopped.store(true, Ordering::SeqCst);
                        errors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push((index, error));
                    }
                }
            });
        }
    });
    let mut errors = errors
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    errors.sort_by_key(|(index, _)| *index);
    errors
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn every_index_runs_once_in_the_given_order_by_one_worker() {
        let seen = Mutex::new(Vec::new());
        let errors = run::<()>(1, &[2, 0, 1], |i| {
            seen.lock().unwrap().push(i);
            Ok(())
        });
        assert!(errors.is_empty());
        assert_eq!(*seen.lock().unwrap(), [2, 0, 1]);
    }

    #[test]
    fn many_workers_run_each_index_exactly_once() {
        let counts: Vec<AtomicUsize> = (0..64).map(|_| AtomicUsize::new(0)).collect();
        let order: Vec<usize> = (0..64).collect();
        let errors = run::<()>(8, &order, |i| {
            counts[i].fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert!(errors.is_empty());
        assert!(counts.iter().all(|c| c.load(Ordering::SeqCst) == 1));
    }

    #[test]
    fn the_first_error_stops_new_starts_and_errors_come_back_by_index() {
        let started = AtomicUsize::new(0);
        let errors = run(1, &[3, 1, 2, 0], |i| {
            started.fetch_add(1, Ordering::SeqCst);
            if i == 1 { Err(format!("job {i}")) } else { Ok(()) }
        });
        assert_eq!(errors, [(1, "job 1".to_owned())]);
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn errors_are_sorted_by_index_whatever_the_order_they_occurred_in() {
        let errors = run(1, &[], |_| Err::<(), _>(()));
        assert!(errors.is_empty());
        let errors = run(4, &[5, 4], |i| Err::<(), _>(i));
        assert!(errors.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert!(!errors.is_empty());
    }
}
