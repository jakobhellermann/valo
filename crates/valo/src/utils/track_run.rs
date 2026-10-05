#![cfg_attr(not(feature = "structured-log"), allow(dead_code))]
pub struct TrackRun<T: PartialEq + Copy, F: Fn(usize, usize, T)> {
    start: usize,
    written: usize,
    value: Option<T>,
    f: F,
}

impl<T: PartialEq + Copy, F: Fn(usize, usize, T)> TrackRun<T, F> {
    pub fn new(f: F) -> Self {
        Self {
            start: 0,
            written: 0,
            value: None,
            f,
        }
    }

    pub fn record(&mut self, value: T) {
        if self.value != Some(value) {
            self.flush();
            self.start = self.written;
            self.value = Some(value);
        }
        self.written += 1;
    }

    fn flush(&self) {
        if let Some(value) = self.value {
            (self.f)(self.start, self.written - self.start, value);
        }
    }
}
impl<T: PartialEq + Copy, F: Fn(usize, usize, T)> Drop for TrackRun<T, F> {
    fn drop(&mut self) {
        self.flush();
    }
}
