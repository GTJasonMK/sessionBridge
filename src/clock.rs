use std::sync::{Arc, RwLock};

use time::{Duration, OffsetDateTime};

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

impl<T: Clock + ?Sized> Clock for &T {
    fn now(&self) -> OffsetDateTime {
        (*self).now()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[derive(Debug, Clone)]
pub struct FakeClock {
    now: Arc<RwLock<OffsetDateTime>>,
}

impl FakeClock {
    pub fn new(now: OffsetDateTime) -> Self {
        Self {
            now: Arc::new(RwLock::new(now)),
        }
    }

    pub fn advance(&self, duration: Duration) {
        let mut now = self.now.write().expect("fake clock lock poisoned");
        *now += duration;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.read().expect("fake clock lock poisoned")
    }
}

pub fn unix_micros(value: OffsetDateTime) -> i64 {
    i64::try_from(value.unix_timestamp_nanos() / 1_000).unwrap_or_else(|_| {
        if value.unix_timestamp_nanos().is_negative() {
            i64::MIN
        } else {
            i64::MAX
        }
    })
}
