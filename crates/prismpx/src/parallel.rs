//! Minimal scoped parallelism without dependencies.

/// Maps `f` over `items`, one thread per item where threads are available
/// (sequentially on WebAssembly).
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    #[cfg(not(target_family = "wasm"))]
    {
        if items.len() > 1 {
            return std::thread::scope(|s| {
                let handles: Vec<_> = items.iter().map(|it| s.spawn(|| f(it))).collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("worker panicked"))
                    .collect()
            });
        }
    }
    items.iter().map(f).collect()
}
