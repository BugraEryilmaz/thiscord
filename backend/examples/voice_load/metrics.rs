use serde::Serialize;

#[derive(Default)]
pub struct Bits(Vec<u64>);
impl Bits {
    pub fn new(size: usize) -> Self {
        Self(vec![0; size.div_ceil(64)])
    }
    pub fn insert(&mut self, n: usize) -> bool {
        let bit = 1 << (n % 64);
        let word = &mut self.0[n / 64];
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }
    pub fn count(&self) -> u64 {
        self.0.iter().map(|w| u64::from(w.count_ones())).sum()
    }
    pub fn missing_from(&self, received: &Self) -> u64 {
        self.0
            .iter()
            .zip(&received.0)
            .map(|(a, b)| u64::from((a & !b).count_ones()))
            .sum()
    }
}

pub struct Stream {
    pub warm: bool,
    pub received: Bits,
    highest: Option<usize>,
    pub duplicates: u64,
    pub reordered: u64,
    pub late: u64,
    pub histogram: Vec<u64>,
}
impl Stream {
    pub fn new(packets: usize) -> Self {
        Self {
            warm: false,
            received: Bits::new(packets),
            highest: None,
            duplicates: 0,
            reordered: 0,
            late: 0,
            histogram: vec![0; 1002],
        }
    }
    pub fn record(&mut self, sequence: usize, delay_ms: u64, late_ms: u64) {
        if !self.received.insert(sequence) {
            self.duplicates += 1;
            return;
        }
        if self.highest.is_some_and(|n| sequence < n) {
            self.reordered += 1;
        }
        self.highest = Some(self.highest.map_or(sequence, |n| n.max(sequence)));
        self.late += u64::from(delay_ms > late_ms);
        self.histogram[delay_ms.min(1001) as usize] += 1;
    }
}

pub struct Stats {
    pub local_candidate: Option<String>,
    pub remote_candidate: Option<String>,
    pub sent: Bits,
    pub attempted: u64,
    pub send_errors: u64,
    pub scheduling_over_20ms: u64,
    pub max_scheduling_lag_us: u64,
    pub invalid: u64,
    pub streams: Vec<Stream>,
}
impl Stats {
    pub fn new(packets: usize, room_size: usize) -> Self {
        Self {
            local_candidate: None,
            remote_candidate: None,
            sent: Bits::new(packets),
            attempted: 0,
            send_errors: 0,
            scheduling_over_20ms: 0,
            max_scheduling_lag_us: 0,
            invalid: 0,
            streams: (0..room_size).map(|_| Stream::new(packets)).collect(),
        }
    }
}

pub fn percentile(histogram: &[u64], percent: u64) -> Option<usize> {
    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return None;
    }
    let target = (total * percent).div_ceil(100);
    let mut sum = 0;
    histogram.iter().position(|n| {
        sum += n;
        sum >= target
    })
}

#[derive(Serialize)]
pub struct Delivery {
    pub receiver: usize,
    pub sender: usize,
    pub expected: u64,
    pub missing: u64,
    pub received_unique: u64,
    pub late: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub path_p99_ms: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_total_loss_and_tail_loss_without_sequence_wrap_or_reorder_bias() {
        let mut sent = Bits::new(70_000);
        for n in [0, 1, 2, 65_536, 69_999] {
            sent.insert(n);
        }
        let mut stream = Stream::new(70_000);
        assert_eq!(sent.missing_from(&stream.received), 5);
        for n in [1, 0, 1, 65_536] {
            stream.record(n, 25, 20);
        }
        assert_eq!(sent.missing_from(&stream.received), 2);
        assert_eq!(stream.received.count(), 3);
        assert_eq!(stream.duplicates, 1);
        assert_eq!(stream.reordered, 1);
        assert_eq!(stream.late, 3);
        assert_eq!(percentile(&stream.histogram, 99), Some(25));
    }
    #[test]
    fn unsent_packets_are_not_network_loss() {
        let mut sent = Bits::new(5);
        let mut received = Bits::new(5);
        sent.insert(2);
        received.insert(2);
        assert_eq!(sent.missing_from(&received), 0);
        assert_eq!(percentile(&[0; 10], 99), None);
    }
}
