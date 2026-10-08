use std::{
    iter,
    num::NonZero,
    panic,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// Work on many items that do not depend on each other, such as the files of a tree, shared out
/// among the cores.
pub trait Spread<T> {
    /// Each item's result, in the items' order.
    fn spread<R: Send>(&self, each: impl Fn(&T) -> R + Sync) -> Vec<R>;
}

impl<T: Sync> Spread<T> for [T] {
    fn spread<R: Send>(&self, each: impl Fn(&T) -> R + Sync) -> Vec<R> {
        let next = AtomicUsize::new(0);
        let cores = thread::available_parallelism().map_or(1, NonZero::get);
        let mut done: Vec<(usize, R)> = thread::scope(|scope| {
            let workers: Vec<_> = (0..cores.min(self.len()))
                .map(|_| {
                    scope.spawn(|| {
                        iter::from_fn(|| {
                            let at = next.fetch_add(1, Ordering::Relaxed);
                            self.get(at).map(|item| (at, each(item)))
                        })
                        .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_else(|panic| panic::resume_unwind(panic)))
                .collect()
        });
        done.sort_by_key(|(at, _)| *at);
        done.into_iter().map(|(_, r)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_come_back_in_the_items_order() {
        let items: Vec<u64> = (0..1000).collect();
        assert_eq!(
            items.spread(|n| n * 2),
            items.iter().map(|n| n * 2).collect::<Vec<_>>()
        );
    }
}
