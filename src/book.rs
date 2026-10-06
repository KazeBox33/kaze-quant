//! 有界的 L2 深度：按价位替换数量，数量为零表示删除。
//! 这不是订单级撮合器；没有外部订单身份或排队信息。
use crate::types::{Price, Quantity, Quote, Side};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub price: Price,
    pub quantity: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepthUpdate {
    pub sequence: u64,
    pub timestamp_ns: u64,
    /// Buy 是 bid 深度，Sell 是 ask 深度。
    pub side: Side,
    pub price: Price,
    pub quantity: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookError {
    InvalidSequence,
    SequenceGap,
    StaleTimestamp,
    InvalidQuantity,
    CrossedBook,
    Capacity,
    InvalidLevels,
    NeedsSnapshot,
}

pub struct OrderBook {
    // 两侧均按价格升序排列；best bid 是末尾，best ask 是开头。
    bids: Vec<Level>,
    asks: Vec<Level>,
    max_levels_per_side: usize,
    last: Option<(u64, u64)>,
}

impl OrderBook {
    pub fn new(max_levels_per_side: usize) -> Result<Self, &'static str> {
        if max_levels_per_side == 0 || max_levels_per_side > 1_000_000 {
            return Err("book capacity must be in 1..=1_000_000 per side");
        }
        Ok(Self {
            bids: Vec::with_capacity(max_levels_per_side),
            asks: Vec::with_capacity(max_levels_per_side),
            max_levels_per_side,
            last: None,
        })
    }
    pub fn bids(&self) -> &[Level] {
        &self.bids
    }
    pub fn asks(&self) -> &[Level] {
        &self.asks
    }
    pub fn best_bid(&self) -> Option<Level> {
        self.bids.last().copied()
    }
    pub fn best_ask(&self) -> Option<Level> {
        self.asks.first().copied()
    }
    pub fn last_sequence(&self) -> Option<u64> {
        self.last.map(|x| x.0)
    }
    pub fn quote(&self) -> Option<Quote> {
        let bid = self.best_bid()?;
        let ask = self.best_ask()?;
        let (sequence, timestamp_ns) = self.last?;
        Some(Quote {
            sequence,
            timestamp_ns,
            bid: bid.price,
            ask: ask.price,
            bid_quantity: bid.quantity,
            ask_quantity: ask.quantity,
        })
    }

    /// 校验完成后才修改。一个明确的全局连续序号流；缺口时需重建整本簿。
    pub fn apply(&mut self, u: DepthUpdate) -> Result<Option<Quote>, BookError> {
        if u.sequence == 0 {
            return Err(BookError::InvalidSequence);
        }
        if let Some((sequence, timestamp)) = self.last {
            if u.sequence <= sequence {
                return Err(BookError::InvalidSequence);
            }
            if sequence.checked_add(1) != Some(u.sequence) {
                return Err(BookError::SequenceGap);
            }
            if u.timestamp_ns < timestamp {
                return Err(BookError::StaleTimestamp);
            }
        }
        if u.quantity > Quantity::MAX {
            return Err(BookError::InvalidQuantity);
        }
        if u.quantity > 0 {
            let crossed = match u.side {
                Side::Buy => self.best_ask().is_some_and(|ask| u.price > ask.price),
                Side::Sell => self.best_bid().is_some_and(|bid| u.price < bid.price),
            };
            if crossed {
                return Err(BookError::CrossedBook);
            }
        }
        let levels = match u.side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let location = levels.binary_search_by_key(&u.price, |level| level.price);
        if u.quantity > 0 && location.is_err() && levels.len() == self.max_levels_per_side {
            return Err(BookError::Capacity);
        }
        match (location, u.quantity) {
            (Ok(index), 0) => {
                levels.remove(index);
            }
            (Ok(index), quantity) => {
                levels[index].quantity = quantity;
            }
            (Err(_), 0) => (), // 删除不存在的档位是幂等更新，但仍消费序号。
            (Err(index), quantity) => {
                levels.insert(
                    index,
                    Level {
                        price: u.price,
                        quantity,
                    },
                );
            }
        }
        self.last = Some((u.sequence, u.timestamp_ns));
        Ok(self.quote())
    }
}

/// 一帧内可有多个档位更新，共享一个序号。中间态可以交叉，最终态必须合法。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelChange {
    pub side: Side,
    pub price: Price,
    pub quantity: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedState {
    AwaitingSnapshot,
    Live,
    NeedsSnapshot,
}

/// 双缓冲使整帧失败不污染主簿；代价是 O(depth) 复制，适配层不冒充低延迟核心。
pub struct BookFeed {
    book: OrderBook,
    scratch_bids: Vec<Level>,
    scratch_asks: Vec<Level>,
    state: FeedState,
    max_changes: usize,
    scratch_keys: Vec<(bool, Price)>,
}
impl BookFeed {
    pub fn new(max_levels_per_side: usize, max_changes: usize) -> Result<Self, &'static str> {
        if max_changes == 0 || max_changes > 1_000_000 {
            return Err("max_changes must be in 1..=1000000");
        }
        Ok(Self {
            book: OrderBook::new(max_levels_per_side)?,
            scratch_bids: Vec::with_capacity(max_levels_per_side),
            scratch_asks: Vec::with_capacity(max_levels_per_side),
            state: FeedState::AwaitingSnapshot,
            max_changes,
            scratch_keys: Vec::with_capacity(max_changes),
        })
    }
    pub fn state(&self) -> FeedState {
        self.state
    }
    /// 只暴露健康行情；缺口后主簿保留供诊断，但不能交易旧报价。
    pub fn quote(&self) -> Option<Quote> {
        (self.state == FeedState::Live)
            .then(|| self.book.quote())
            .flatten()
    }
    pub fn book(&self) -> &OrderBook {
        &self.book
    }
    pub fn snapshot(
        &mut self,
        sequence: u64,
        timestamp_ns: u64,
        bids: &[Level],
        asks: &[Level],
    ) -> Result<Option<Quote>, BookError> {
        if sequence == 0
            || self
                .book
                .last_sequence()
                .is_some_and(|last| sequence <= last)
        {
            return Err(BookError::InvalidSequence);
        }
        if self.book.last.is_some_and(|(_, ts)| timestamp_ns < ts) {
            return Err(BookError::StaleTimestamp);
        }
        let cap = self.book.max_levels_per_side;
        if bids.len() > cap || asks.len() > cap {
            return Err(BookError::Capacity);
        }
        for levels in [bids, asks] {
            if levels
                .iter()
                .any(|l| l.quantity == 0 || l.quantity > Quantity::MAX)
            {
                return Err(BookError::InvalidQuantity);
            }
            if levels.windows(2).any(|pair| pair[0].price >= pair[1].price) {
                return Err(BookError::InvalidLevels);
            }
        }
        Self::validate_spread(bids, asks)?;
        self.book.bids.clear();
        self.book.bids.extend_from_slice(bids);
        self.book.asks.clear();
        self.book.asks.extend_from_slice(asks);
        self.book.last = Some((sequence, timestamp_ns));
        self.state = FeedState::Live;
        Ok(self.quote())
    }
    pub fn delta(
        &mut self,
        sequence: u64,
        timestamp_ns: u64,
        changes: &[LevelChange],
    ) -> Result<Option<Quote>, BookError> {
        if self.state != FeedState::Live {
            return Err(BookError::NeedsSnapshot);
        }
        let (last, ts) = self.book.last.expect("live feed has snapshot");
        if sequence <= last {
            return Err(BookError::InvalidSequence);
        }
        if last.checked_add(1) != Some(sequence) {
            self.state = FeedState::NeedsSnapshot;
            return Err(BookError::SequenceGap);
        }
        if timestamp_ns < ts {
            return Err(BookError::StaleTimestamp);
        }
        if changes.len() > self.max_changes {
            return Err(BookError::Capacity);
        }
        self.scratch_bids.clear();
        self.scratch_bids.extend_from_slice(&self.book.bids);
        self.scratch_asks.clear();
        self.scratch_asks.extend_from_slice(&self.book.asks);
        // 先处理删除，再处理增加/替换，允许满容量簿在同一帧替换档位。
        // 同帧重复 side/price 禁止，避免把交换顺序误当成协议定义的 last-write-wins。
        self.scratch_keys.clear();
        self.scratch_keys.extend(
            changes
                .iter()
                .map(|c| (matches!(c.side, Side::Sell), c.price)),
        );
        self.scratch_keys.sort_unstable();
        if self.scratch_keys.windows(2).any(|p| p[0] == p[1]) {
            return Err(BookError::InvalidLevels);
        }
        for zero in [true, false] {
            for change in changes.iter().filter(|c| (c.quantity == 0) == zero) {
                if change.quantity > Quantity::MAX {
                    return Err(BookError::InvalidQuantity);
                }
                let levels = match change.side {
                    Side::Buy => &mut self.scratch_bids,
                    Side::Sell => &mut self.scratch_asks,
                };
                match (
                    levels.binary_search_by_key(&change.price, |l| l.price),
                    change.quantity,
                ) {
                    (Ok(i), 0) => {
                        levels.remove(i);
                    }
                    (Ok(i), quantity) => levels[i].quantity = quantity,
                    (Err(_), 0) => (),
                    (Err(i), quantity) => {
                        if levels.len() == self.book.max_levels_per_side {
                            return Err(BookError::Capacity);
                        }
                        levels.insert(
                            i,
                            Level {
                                price: change.price,
                                quantity,
                            },
                        );
                    }
                }
            }
        }
        Self::validate_spread(&self.scratch_bids, &self.scratch_asks)?;
        std::mem::swap(&mut self.book.bids, &mut self.scratch_bids);
        std::mem::swap(&mut self.book.asks, &mut self.scratch_asks);
        self.book.last = Some((sequence, timestamp_ns));
        Ok(self.quote())
    }
    fn validate_spread(bids: &[Level], asks: &[Level]) -> Result<(), BookError> {
        if let (Some(b), Some(a)) = (bids.last(), asks.first())
            && b.price > a.price
        {
            return Err(BookError::CrossedBook);
        }
        Ok(())
    }
}
