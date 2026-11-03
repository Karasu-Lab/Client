#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Instant, SystemTime, UNIX_EPOCH};

pub fn now_local() -> time::OffsetDateTime {
    #[cfg(not(target_arch = "wasm32"))]
    return time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    #[cfg(target_arch = "wasm32")]
    {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        time::OffsetDateTime::from_unix_timestamp(seconds)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
    }
}
