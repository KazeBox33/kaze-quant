//! 交易所十进制边界：核算使用整数；不经 f64，不静默舍入委托价格/数量。
use crate::paper::PaperError;
pub const SCALE: i128 = 100_000_000;
pub fn parse(value: &str) -> Result<i128, PaperError> {
    if value.len() > 40 || value.is_empty() {
        return Err("invalid decimal length".into());
    }
    let mut parts = value.split('.');
    let whole = parts.next().unwrap();
    let fraction = parts.next().unwrap_or("");
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || parts.next().is_some()
    {
        return Err("decimal must be unsigned plain notation".into());
    }
    if fraction.len() > 8 && fraction[8..].bytes().any(|b| b != b'0') {
        return Err("decimal precision exceeds 8 places".into());
    }
    let used = &fraction[..fraction.len().min(8)];
    let a: i128 = whole
        .parse()
        .map_err(|_| PaperError("decimal overflow".into()))?;
    let b: i128 = if used.is_empty() {
        0
    } else {
        used.parse()
            .map_err(|_| PaperError("decimal overflow".into()))?
    };
    let n = a
        .checked_mul(SCALE)
        .and_then(|n| n.checked_add(b * 10i128.pow((8 - used.len()) as u32)))
        .ok_or("decimal overflow")?;
    if n > 10i128.pow(30) {
        return Err("decimal exceeds economic bound".into());
    }
    Ok(n)
}
pub fn format(n: i128) -> Result<String, PaperError> {
    if !(0..=10i128.pow(30)).contains(&n) {
        return Err("decimal outside economic bound".into());
    }
    Ok(format!("{}.{:08}", n / SCALE, n % SCALE))
}
/// 输入行情数量可向下量化；下单数量必须精确表示。
pub fn rescale(n: i128, scale: u64, exact: bool) -> Result<u64, PaperError> {
    let x = n.checked_mul(i128::from(scale)).ok_or("scale overflow")?;
    if exact && x % SCALE != 0 {
        return Err("decimal does not fit instrument scale".into());
    }
    u64::try_from(x / SCALE).map_err(|_| "rescaled decimal overflow".into())
}
