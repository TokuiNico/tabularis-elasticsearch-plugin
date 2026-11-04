//! Helpers shared by tests.

use std::{future::Future, sync::LazyLock};
use tokio::runtime::Runtime;

/// One runtime for every test that talks to Elasticsearch.
///
/// `es::pool` caches transports for the whole process, and a cached
/// connection belongs to the runtime that opened it. Under `#[tokio::test]`
/// each test has its own runtime, so a test could pick up a connection whose
/// runtime ended with an earlier test and fail with "runtime dropped the
/// dispatch task". The plugin itself runs on a single runtime.
static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| Runtime::new().expect("test runtime"));

pub fn block_on<F: Future>(future: F) -> F::Output {
    RUNTIME.block_on(future)
}
