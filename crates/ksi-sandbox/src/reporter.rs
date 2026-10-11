//! Size-bounded execution report accumulator.
//!
//! Port of Python's `util.programming.Reporter`: appending is capped at
//! `max_size` bytes and [`Reporter::report_truncated`] returns the report
//! with a `[TRUNCATED …]` marker when the cap was exceeded.

/// Accumulates an execution report, capped at `max_size` bytes.
#[derive(Debug, Default)]
pub struct Reporter {
    report_start: String,
    report_end: String,
    max_size: usize,
    truncated_length: usize,
}

impl Reporter {
    /// Create an empty reporter with the given byte cap.
    pub fn new(max_size: usize) -> Self {
        Self {
            report_start: String::new(),
            report_end: String::new(),
            max_size,
            truncated_length: 0,
        }
    }

    /// Full report, up to the maximal size, without a truncation warning.
    pub fn report(&self) -> &str {
        &self.report_start
    }

    /// The report, possibly truncated to `max_size` bytes with a
    /// `[TRUNCATED N CHARACTERS]` marker when content was dropped.
    pub fn report_truncated(&self) -> String {
        let byte_size = self.report_start.len();
        if byte_size < self.max_size {
            return self.report_start.clone();
        }
        let marker = format!(" [TRUNCATED {} CHARACTERS] ", self.truncated_length);
        let keep = self.max_size.saturating_sub(marker.len());
        format!("{}{}{}", &self.report_start[..keep], marker, self.report_end)
    }

    /// Append a string (mirrors Python's `reporter += …`).
    pub fn push(&mut self, other: &str) {
        let max_new_length = self.max_size.saturating_sub(self.report_start.len());
        if max_new_length == 0 {
            self.truncated_length += other.len();
            // keep both the beginning and the end of the output, strip the middle
            if self.report_start.len().abs_diff(self.report_end.len()) > 10 {
                let keep = self.max_size.min(other.len());
                self.report_start.truncate(keep);
                let end_keep = self.max_size.min(other.len());
                self.report_end.push_str(&other[..end_keep]);
            }
            return;
        }
        self.truncated_length += other.len().saturating_sub(max_new_length);
        self.report_start.push_str(&other[..max_new_length.min(other.len())]);
    }
}

impl std::ops::AddAssign<&str> for Reporter {
    fn add_assign(&mut self, rhs: &str) {
        self.push(rhs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_truncates() {
        let mut r = Reporter::new(10);
        r += "0123456789ABCDEF";
        assert!(r.report().len() <= 10);
        let t = r.report_truncated();
        assert!(t.contains("[TRUNCATED"));
        assert!(t.len() <= 10 + "[TRUNCATED 16 CHARACTERS] ".len());
    }

    #[test]
    fn small_report_untouched() {
        let mut r = Reporter::new(100);
        r += "hello";
        assert_eq!(r.report_truncated(), "hello");
    }
}
