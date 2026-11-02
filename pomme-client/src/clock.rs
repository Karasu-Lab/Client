//! Wall-clock and monotonic time. The browser has no `std::time` clock, so
//! there they come from `performance.now()` and `Date.now()` through web-time.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Instant, SystemTime, UNIX_EPOCH};
