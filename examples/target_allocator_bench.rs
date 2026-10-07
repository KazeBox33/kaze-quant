//! 同一有界输入逐量枚举的数量oracle与整数闭式分配；不是上游平台对照。
use kaze_quant::{
    decimal::SCALE,
    external_target::{AllocationInput, allocate},
};
use std::{hint::black_box, time::Instant};
fn sample(n: usize) -> AllocationInput {
    let step = 10000;
    let net = (n % 129) as i128 * step;
    AllocationInput {
        net_base: net,
        free_base: net * (n % 5) as i128 / 4,
        free_quote: (n % 129) as i128 * 3 * step,
        target: ((n * 17) % 129) as i128 * step,
        tolerance: (n % 3) as i128 * step,
        price: 100000001 + (n % 7) as i128 * 50000000,
        step,
        min_total: 3 * step,
        max_gross: 128 * step,
        base_fee_reserve: (n % 5) as i128 * 100,
        quote_fee_reserve: (n % 11) as i128 * 100,
    }
}
fn enumerated_quantity(i: AllocationInput) -> i128 {
    let mut best = 0;
    if (i.net_base - i.target).abs() <= i.tolerance {
        return best;
    }
    for lot in 1..=i.max_gross / i.step {
        let q = lot * i.step;
        if q < i.min_total || i.free_quote < i.quote_fee_reserve {
            continue;
        }
        let buy = i.target > i.net_base;
        let cost = (q * i.price + SCALE - 1) / SCALE;
        if if buy {
            q >= i.base_fee_reserve
                && i.net_base + q <= i.target
                && cost + i.quote_fee_reserve <= i.free_quote
        } else {
            q + i.base_fee_reserve <= i.free_base && i.net_base - q - i.base_fee_reserve >= i.target
        } {
            best = q;
        }
    }
    best
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "1000000".into())
        .parse::<usize>()?;
    if !(10000..=10000000).contains(&count) {
        return Err("count must be 10000..10000000".into());
    }
    let inputs: Vec<_> = (0..4096).map(sample).collect();
    for &i in &inputs {
        if allocate(i)?.gross != enumerated_quantity(i) {
            return Err("quantity oracle differs".into());
        }
    }
    let mut rows = vec![];
    for round in 0..7 {
        for mode in if round % 2 == 0 {
            ["formula", "enumeration"]
        } else {
            ["enumeration", "formula"]
        } {
            let start = Instant::now();
            let mut checksum = 0u128;
            for n in 0..count {
                let i = black_box(inputs[n % inputs.len()]);
                let q = if mode == "formula" {
                    black_box(allocate(i)?).gross
                } else {
                    enumerated_quantity(i)
                };
                checksum = checksum.wrapping_mul(31).wrapping_add(black_box(q) as u128);
            }
            rows.push(serde_json::json!({"round":round,"mode":mode,"allocations":count,"elapsed_ns":start.elapsed().as_nanos(),"checksum":checksum.to_string()}));
        }
    }
    let first = rows[0]["checksum"].clone();
    if rows.iter().any(|r| r["checksum"] != first) {
        return Err("timed quantity checksum differs".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"schema_version":1,"success":true,"input_samples":4096,"max_enumerated_lots":128,"rounds_each":7,"runs":rows,"scope":"allocation component only; production formula includes bound checks and diagnostics; naive own quantity oracle scans up to128 lots and only returns gross; no CSV/SQL/JSON/network/strategy economics/upstream platform benchmark"})
        )?
    );
    Ok(())
}
