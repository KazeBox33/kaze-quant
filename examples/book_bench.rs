//! 同一受校验 L2 更新流：有序 Vec 与独立 BTreeMap 对照。
use kaze_quant::book::{BookError, DepthUpdate, OrderBook};
use kaze_quant::types::*;
use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

struct Reference {
    bids: BTreeMap<Price, u64>,
    asks: BTreeMap<Price, u64>,
    last: Option<(u64, u64)>,
    capacity: usize,
}
impl Reference {
    fn apply(&mut self, u: DepthUpdate) -> Result<Option<Quote>, BookError> {
        if u.sequence == 0 {
            return Err(BookError::InvalidSequence);
        }
        if let Some((seq, ts)) = self.last {
            if u.sequence <= seq {
                return Err(BookError::InvalidSequence);
            }
            if seq.checked_add(1) != Some(u.sequence) {
                return Err(BookError::SequenceGap);
            }
            if u.timestamp_ns < ts {
                return Err(BookError::StaleTimestamp);
            }
        }
        if u.quantity > Quantity::MAX {
            return Err(BookError::InvalidQuantity);
        }
        if u.quantity > 0 {
            let crossed = match u.side {
                Side::Buy => self
                    .asks
                    .first_key_value()
                    .is_some_and(|(&p, _)| u.price > p),
                Side::Sell => self
                    .bids
                    .last_key_value()
                    .is_some_and(|(&p, _)| u.price < p),
            };
            if crossed {
                return Err(BookError::CrossedBook);
            }
        }
        let side = if u.side == Side::Buy {
            &mut self.bids
        } else {
            &mut self.asks
        };
        if u.quantity > 0 && side.len() == self.capacity && !side.contains_key(&u.price) {
            return Err(BookError::Capacity);
        }
        if u.quantity == 0 {
            side.remove(&u.price);
        } else {
            side.insert(u.price, u.quantity);
        }
        self.last = Some((u.sequence, u.timestamp_ns));
        Ok(self
            .bids
            .last_key_value()
            .zip(self.asks.first_key_value())
            .map(|((&bid, &bid_quantity), (&ask, &ask_quantity))| Quote {
                sequence: u.sequence,
                timestamp_ns: u.timestamp_ns,
                bid,
                ask,
                bid_quantity,
                ask_quantity,
            }))
    }
}

fn checksum(q: Option<Quote>) -> u128 {
    q.map_or(0, |q| {
        u128::from(q.bid.units())
            + u128::from(q.ask.units())
            + u128::from(q.bid_quantity)
            + u128::from(q.ask_quantity)
    })
}
fn feed(size: usize, count: usize) -> Vec<DepthUpdate> {
    let mut updates = Vec::with_capacity(2 * size + count);
    for side in [Side::Buy, Side::Sell] {
        for index in 0..size {
            let price = if side == Side::Buy {
                10_000 - size as u64 + index as u64
            } else {
                10_001 + index as u64
            };
            let sequence = updates.len() as u64 + 1;
            updates.push(DepthUpdate {
                sequence,
                timestamp_ns: sequence,
                side,
                price: Price::new(price).unwrap(),
                quantity: 10,
            });
        }
    }
    let mut seed = 123u64;
    for _ in 0..count {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let side = if seed & 1 == 0 { Side::Buy } else { Side::Sell };
        let index = (seed >> 16) % size as u64;
        let price = if side == Side::Buy {
            10_000 - size as u64 + index
        } else {
            10_001 + index
        };
        let sequence = updates.len() as u64 + 1;
        updates.push(DepthUpdate {
            sequence,
            timestamp_ns: sequence,
            side,
            price: Price::new(price).unwrap(),
            quantity: (seed >> 32) % 8,
        });
    }
    updates
}
fn run(size: usize, updates: &[DepthUpdate], vector: bool) -> (u128, u128) {
    let mut book = OrderBook::new(size).unwrap();
    let mut reference = Reference {
        bids: BTreeMap::new(),
        asks: BTreeMap::new(),
        last: None,
        capacity: size,
    };
    for &update in &updates[..2 * size] {
        if vector {
            book.apply(update).unwrap();
        } else {
            reference.apply(update).unwrap();
        }
    }
    let mut total = 0u128;
    let start = Instant::now();
    for &update in &updates[2 * size..] {
        let quote = if vector {
            book.apply(black_box(update)).unwrap()
        } else {
            reference.apply(black_box(update)).unwrap()
        };
        total += checksum(quote);
    }
    (start.elapsed().as_nanos(), black_box(total))
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run with --release");
        std::process::exit(1);
    }
    let repeats: usize = std::env::args()
        .nth(1)
        .map_or(Ok(7), |s| s.parse())
        .expect("integer repeats");
    assert!((3..=31).contains(&repeats));
    println!(
        "workload,size,variant,samples,min_batch_ns,median_batch_ns,max_batch_ns,orders_examined,checksum"
    );
    for size in [16, 64, 1024, 8192] {
        let updates = feed(size, 100_000);
        // 对整条流逐输出核对，包括初始构簿；不计入性能时间。
        let mut book = OrderBook::new(size).unwrap();
        let mut reference = Reference {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            last: None,
            capacity: size,
        };
        for &u in &updates {
            assert_eq!(book.apply(u), reference.apply(u));
        }
        let a = run(size, &updates, true);
        let b = run(size, &updates, false);
        assert_eq!(a.1, b.1);
        let (mut fast, mut slow) = (Vec::new(), Vec::new());
        for repeat in 0..repeats {
            let (a, b) = if repeat % 2 == 0 {
                (run(size, &updates, true), run(size, &updates, false))
            } else {
                let b = run(size, &updates, false);
                (run(size, &updates, true), b)
            };
            assert_eq!(a.1, b.1);
            fast.push(a.0);
            slow.push(b.0);
        }
        for (name, mut samples) in [("sorted_vec", fast), ("btree", slow)] {
            samples.sort_unstable();
            println!(
                "depth_100000_updates,{size},{name},{repeats},{},{},{},0,{}",
                samples[0],
                samples[repeats / 2],
                samples[repeats - 1],
                a.1
            );
        }
    }
}
