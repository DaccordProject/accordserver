use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// Accord epoch: 2024-01-01T00:00:00Z
const EPOCH: u64 = 1_704_067_200_000;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static LAST_TIMESTAMP: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock went backwards")
        .as_millis() as u64
}

pub fn generate() -> String {
    let mut timestamp = now_ms() - EPOCH;
    let last = LAST_TIMESTAMP.load(Ordering::SeqCst);

    if timestamp == last {
        let seq = SEQUENCE.fetch_add(1, Ordering::SeqCst) & 0xFFF;
        if seq == 0 {
            // Sequence overflow, wait for next millisecond
            while timestamp <= last {
                timestamp = now_ms() - EPOCH;
            }
        }
        LAST_TIMESTAMP.store(timestamp, Ordering::SeqCst);
        let id = (timestamp << 22) | seq;
        id.to_string()
    } else {
        LAST_TIMESTAMP.store(timestamp, Ordering::SeqCst);
        SEQUENCE.store(1, Ordering::SeqCst);
        let id = timestamp << 22;
        id.to_string()
    }
}

pub fn timestamp_of(id: &str) -> Option<u64> {
    let num: u64 = id.parse().ok()?;
    Some((num >> 22) + EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generates_unique_ids() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b);
    }

    #[test]
    fn test_ids_are_parseable() {
        let id = generate();
        assert!(id.parse::<u64>().is_ok());
    }

    #[test]
    fn test_timestamp_extraction() {
        let id = generate();
        let ts = timestamp_of(&id).unwrap();
        let now = now_ms();
        assert!(ts <= now && ts > now - 1000);
    }

    #[test]
    fn test_monotonically_increasing() {
        let ids: Vec<u64> = (0..100)
            .map(|_| generate().parse::<u64>().unwrap())
            .collect();
        for w in ids.windows(2) {
            assert!(w[0] < w[1], "IDs should be monotonically increasing");
        }
    }
}
