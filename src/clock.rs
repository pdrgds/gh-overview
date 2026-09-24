use chrono::{DateTime, Utc};

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct FakeClock(std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>);

#[cfg(test)]
impl FakeClock {
    pub fn at(t: DateTime<Utc>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(t)))
    }

    pub fn advance(&self, d: chrono::Duration) {
        *self.0.lock().unwrap() += d;
    }
}

#[cfg(test)]
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}
