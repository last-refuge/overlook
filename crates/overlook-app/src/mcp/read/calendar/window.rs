use super::{invalid, Failure};
use overlook_core::{tz, Event};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_SPAN_MS: i64 = 366 * 24 * 60 * 60 * 1000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Window {
    pub start: i64,
    pub end: i64,
}

impl Window {
    pub fn new(start: i64, end: i64) -> Self {
        Self { start, end }
    }

    pub fn event(event: &Event) -> Self {
        Self::new(event.start, event.end.max(event.start.saturating_add(1)))
    }

    pub fn parse(start: &str, end: &str) -> Result<Self, Failure> {
        let (Some(start), Some(end)) = (timestamp(start), timestamp(end)) else {
            return Err(invalid("Calendar range requires RFC 3339 date-times with UTC offsets and at most millisecond precision"));
        };
        if start >= end || end - start > MAX_SPAN_MS {
            return Err(invalid(
                "Calendar range must be positive and at most 366 elapsed days",
            ));
        }
        Ok(Self::new(start, end))
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Self::new(start, end))
    }

    pub fn value(&self) -> Value {
        json!({"start": tz::to_rfc3339(self.start), "end": tz::to_rfc3339(self.end)})
    }
}

impl From<(i64, i64)> for Window {
    fn from((start, end): (i64, i64)) -> Self {
        Self::new(start, end)
    }
}

fn timestamp(input: &str) -> Option<i64> {
    // The repo parser also accepts zoned date-times and non-RFC 3339 forms.
    let remainder = match input.as_bytes() {
        [_, _, _, _, b'-', _, _, b'-', _, _, b'T' | b't', _, _, b':', _, _, b':', _, _, rest @ ..] => {
            rest
        }
        _ => return None,
    };
    let offset = if let Some(fraction) = remainder.strip_prefix(b".") {
        let len = fraction.iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=3).contains(&len) {
            return None;
        }
        &fraction[len..]
    } else {
        remainder
    };
    // RFC 3339 gives -00:00 a special meaning, but it still gives a UTC instant.
    match offset {
        [b'Z' | b'z'] | [b'+' | b'-', _, _, b':', _, _] => tz::parse_rfc3339(input),
        _ => None,
    }
}

pub(super) fn merge(mut windows: Vec<Window>) -> Vec<Window> {
    windows.sort_unstable_by_key(|w| w.start);
    let mut merged: Vec<Window> = Vec::new();
    for window in windows {
        match merged.last_mut() {
            Some(last) if last.end >= window.start => last.end = last.end.max(window.end),
            _ => merged.push(window),
        }
    }
    merged
}

pub(super) fn gaps(range: Window, covered: &[Window]) -> Vec<Window> {
    let mut result = Vec::new();
    let mut start = range.start;
    for window in covered {
        if start < window.start {
            result.push(Window::new(start, window.start));
        }
        start = window.end;
    }
    if start < range.end {
        result.push(Window::new(start, range.end));
    }
    result
}

pub(super) fn availability(range: Window, covered: &[Window], busy: Vec<Window>) -> Vec<Value> {
    let busy = merge(busy);
    let mut points = vec![range.start, range.end];
    for w in busy.iter().chain(covered) {
        points.extend([w.start, w.end]);
    }
    points.sort_unstable();
    points.dedup();
    let mut busy = busy.iter().peekable();
    let mut covered = covered.iter().peekable();
    let mut slots: Vec<(Window, &str)> = Vec::new();
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        while busy.peek().is_some_and(|w| w.end <= start) {
            busy.next();
        }
        while covered.peek().is_some_and(|w| w.end <= start) {
            covered.next();
        }
        let status = if busy.peek().is_some_and(|w| w.start <= start) {
            "busy"
        } else if covered.peek().is_some_and(|w| w.start <= start) {
            "free_in_cache"
        } else {
            "unknown"
        };
        match slots.last_mut() {
            Some((last, previous)) if *previous == status => last.end = end,
            _ => slots.push((Window::new(start, end), status)),
        }
    }
    slots
        .into_iter()
        .map(|(window, status)| {
            let mut result = window.value();
            result["status"] = json!(status);
            result
        })
        .collect()
}
