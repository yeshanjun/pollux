pub(crate) mod endpoints;
pub mod ops;
pub mod types;
pub mod utils;

use backon::ExponentialBuilder;
use std::{sync::LazyLock, time::Duration};

pub(crate) static OAUTH_RETRY_POLICY: LazyLock<ExponentialBuilder> = LazyLock::new(|| {
    ExponentialBuilder::default()
        .with_min_delay(Duration::from_secs(1))
        .with_max_delay(Duration::from_secs(3))
        .with_max_times(3)
        .with_jitter()
});
