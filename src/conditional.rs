//! 本地条件意图：等待阶段不冻结账户；只有激活的普通订单才能成交。
//! 价格、到期和OCO索引是可重建投影，经济身份/状态随Paper检查点提交。
use crate::{engine::MAX_LIFETIME_ORDERS, paper::RiskReject, types::*};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConditionalId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reference {
    Bid,
    Ask,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    AboveOrEqual,
    BelowOrEqual,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalRequest {
    pub reference: Reference,
    pub direction: Direction,
    pub trigger: Price,
    pub order: OrderRequest,
    #[serde(default)]
    pub expires_at_ns: Option<u64>,
    #[serde(default)]
    pub oco_group: Option<u64>,
}
impl ConditionalRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .oco_group
            .is_some_and(|g| g == 0 || g > MAX_LIFETIME_ORDERS)
        {
            return Err("invalid OCO group");
        }
        Ok(())
    }
    pub fn matches(&self, q: Quote) -> bool {
        let price = match self.reference {
            Reference::Bid => q.bid,
            Reference::Ask => q.ask,
        };
        match self.direction {
            Direction::AboveOrEqual => price >= self.trigger,
            Direction::BelowOrEqual => price <= self.trigger,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "reason", rename_all = "snake_case")]
pub enum ConditionalReject {
    Capacity,
    Expired,
    InvalidRequest,
    InvalidClock,
    Risk(RiskReject),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "reason", rename_all = "snake_case")]
pub enum ActivationReject {
    Risk(RiskReject),
    Engine(RejectReason),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelReason {
    Operator,
    OcoPeerAccepted,
    Expired,
    Halted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConditionalEvent {
    Accepted {
        conditional_id: ConditionalId,
        request: ConditionalRequest,
    },
    Rejected {
        request: ConditionalRequest,
        reason: ConditionalReject,
    },
    Triggered {
        conditional_id: ConditionalId,
        order_id: OrderId,
        quote_sequence: u64,
    },
    ActivationRejected {
        conditional_id: ConditionalId,
        reason: ActivationReject,
        quote_sequence: u64,
    },
    Cancelled {
        conditional_id: ConditionalId,
        reason: CancelReason,
    },
    CancelMissing {
        conditional_id: ConditionalId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    pub id: ConditionalId,
    pub request: ConditionalRequest,
    pub submitted_quote_sequence: u64,
    pub submitted_at_ns: u64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalMetrics {
    pub accepted: u64,
    pub rejected: u64,
    pub triggered: u64,
    pub activation_rejected: u64,
    pub cancelled: u64,
    pub expired: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionalSnapshot {
    next_id: u64,
    pending: Vec<Waiting>,
    metrics: ConditionalMetrics,
    last_sequence: Option<u64>,
    last_time_ns: u64,
}
/// Adaptive是平台默认；Scan/Indexed保留算法对照，共享激活逻辑非独立经济oracle。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TriggerPolicy {
    Indexed,
    Scan,
    Adaptive,
}
pub struct Selection {
    pub ids: Vec<ConditionalId>,
    pub examined: usize,
}
pub struct ConditionalBook {
    capacity: usize,
    next_id: u64,
    pending: BTreeMap<ConditionalId, Waiting>,
    prices: [BTreeSet<(u64, ConditionalId)>; 4],
    expiry: BTreeSet<(u64, ConditionalId)>,
    groups: BTreeMap<u64, BTreeSet<ConditionalId>>,
    metrics: ConditionalMetrics,
    last_sequence: Option<u64>,
    last_time_ns: u64,
}
impl ConditionalBook {
    pub fn new(capacity: usize) -> Result<Self, &'static str> {
        if capacity == 0 || capacity > 4096 {
            return Err("conditional capacity must be in 1..=4096");
        }
        Ok(Self {
            capacity,
            next_id: 1,
            pending: BTreeMap::new(),
            prices: std::array::from_fn(|_| BTreeSet::new()),
            expiry: BTreeSet::new(),
            groups: BTreeMap::new(),
            metrics: ConditionalMetrics::default(),
            last_sequence: None,
            last_time_ns: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.pending.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &Waiting> {
        self.pending.values()
    }
    pub fn get(&self, id: ConditionalId) -> Option<&Waiting> {
        self.pending.get(&id)
    }
    pub fn metrics(&self) -> ConditionalMetrics {
        self.metrics
    }
    fn index(r: ConditionalRequest) -> usize {
        match (r.reference, r.direction) {
            (Reference::Bid, Direction::AboveOrEqual) => 0,
            (Reference::Bid, Direction::BelowOrEqual) => 1,
            (Reference::Ask, Direction::AboveOrEqual) => 2,
            (Reference::Ask, Direction::BelowOrEqual) => 3,
        }
    }
    fn insert(&mut self, w: Waiting) {
        self.prices[Self::index(w.request)].insert((w.request.trigger.units(), w.id));
        if let Some(t) = w.request.expires_at_ns {
            self.expiry.insert((t, w.id));
        }
        if let Some(g) = w.request.oco_group {
            self.groups.entry(g).or_default().insert(w.id);
        }
        self.pending.insert(w.id, w);
    }
    fn remove(&mut self, id: ConditionalId) -> Option<Waiting> {
        let w = self.pending.remove(&id)?;
        self.prices[Self::index(w.request)].remove(&(w.request.trigger.units(), id));
        if let Some(t) = w.request.expires_at_ns {
            self.expiry.remove(&(t, id));
        }
        if let Some(g) = w.request.oco_group {
            let members = self.groups.get_mut(&g).expect("indexed OCO group");
            members.remove(&id);
            if members.is_empty() {
                self.groups.remove(&g);
            }
        }
        Some(w)
    }
    pub fn record_rejection(&mut self) {
        self.metrics.rejected = (self.metrics.rejected + 1).min(MAX_LIFETIME_ORDERS * 64);
    }
    pub fn submit(
        &mut self,
        r: ConditionalRequest,
        q: Quote,
        now: u64,
    ) -> Result<ConditionalId, ConditionalReject> {
        let rejection = if r.validate().is_err() {
            Some(ConditionalReject::InvalidRequest)
        } else if q.validate().is_err()
            || q.timestamp_ns > now
            || now < self.last_time_ns
            || self.last_sequence.is_some_and(|s| q.sequence < s)
        {
            Some(ConditionalReject::InvalidClock)
        } else if r.expires_at_ns.is_some_and(|t| t <= now) {
            Some(ConditionalReject::Expired)
        } else if self.len() >= self.capacity || self.next_id > MAX_LIFETIME_ORDERS {
            Some(ConditionalReject::Capacity)
        } else {
            None
        };
        if let Some(reason) = rejection {
            self.record_rejection();
            return Err(reason);
        }
        let id = ConditionalId(self.next_id);
        self.next_id += 1;
        self.metrics.accepted += 1;
        self.insert(Waiting {
            id,
            request: r,
            submitted_quote_sequence: q.sequence,
            submitted_at_ns: now,
        });
        Ok(id)
    }
    pub fn cancel(&mut self, id: ConditionalId, reason: CancelReason) -> ConditionalEvent {
        if self.remove(id).is_some() {
            if reason == CancelReason::Expired {
                self.metrics.expired += 1;
            } else {
                self.metrics.cancelled += 1;
            }
            ConditionalEvent::Cancelled {
                conditional_id: id,
                reason,
            }
        } else {
            ConditionalEvent::CancelMissing { conditional_id: id }
        }
    }
    pub fn cancel_all(&mut self, reason: CancelReason) -> Vec<ConditionalEvent> {
        let ids: Vec<_> = self.pending.keys().copied().collect();
        ids.into_iter().map(|id| self.cancel(id, reason)).collect()
    }
    pub fn expire(&mut self, now: u64) -> Result<Vec<ConditionalEvent>, &'static str> {
        if now < self.last_time_ns {
            return Err("conditional clock regressed");
        }
        self.last_time_ns = now;
        let ids: Vec<_> = self
            .expiry
            .range(..=(now, ConditionalId(u64::MAX)))
            .map(|&(_, id)| id)
            .collect();
        // 同时到期按意图提交顺序发回报，不按到期价格索引顺序。
        let mut ids = ids;
        ids.sort_unstable();
        Ok(ids
            .into_iter()
            .map(|id| self.cancel(id, CancelReason::Expired))
            .collect())
    }
    pub fn select(&self, q: Quote, policy: TriggerPolicy) -> Selection {
        // 完全跨过的子树用首/尾键快速识别，避免密集候选逐ID查树和排序。
        // 这是密度下界，不是精确计数；部分跨越时仍走索引，不扫描两次。
        if policy == TriggerPolicy::Adaptive {
            let prices = [q.bid.units(), q.bid.units(), q.ask.units(), q.ask.units()];
            let fully_crossed: usize = self
                .prices
                .iter()
                .enumerate()
                .filter_map(|(i, tree)| {
                    let crossed = if i % 2 == 0 {
                        tree.last().is_some_and(|&(p, _)| p <= prices[i])
                    } else {
                        tree.first().is_some_and(|&(p, _)| p >= prices[i])
                    };
                    crossed.then_some(tree.len())
                })
                .sum();
            return self.select(
                q,
                if fully_crossed > self.len() / 4 {
                    TriggerPolicy::Scan
                } else {
                    TriggerPolicy::Indexed
                },
            );
        }
        let mut ids = Vec::new();
        let examined;
        match policy {
            TriggerPolicy::Scan => {
                examined = self.len();
                ids.extend(
                    self.pending
                        .values()
                        .filter(|w| w.submitted_quote_sequence < q.sequence && w.request.matches(q))
                        .map(|w| w.id),
                );
            }
            TriggerPolicy::Indexed => {
                for (i, price) in [q.bid.units(), q.bid.units(), q.ask.units(), q.ask.units()]
                    .into_iter()
                    .enumerate()
                {
                    if i % 2 == 0 {
                        ids.extend(
                            self.prices[i]
                                .range(..=(price, ConditionalId(u64::MAX)))
                                .map(|&(_, id)| id),
                        );
                    } else {
                        ids.extend(
                            self.prices[i]
                                .range((price, ConditionalId(0))..)
                                .map(|&(_, id)| id),
                        );
                    }
                }
                examined = ids.len();
                ids.retain(|id| self.pending[id].submitted_quote_sequence < q.sequence);
                ids.sort_unstable();
            }
            TriggerPolicy::Adaptive => unreachable!("resolved before selection"),
        }
        Selection { ids, examined }
    }
    /// 到期优先于触发。激活失败消费本意图，不盲重试；成功接受才撤销OCO等待同伴。
    pub fn on_quote(
        &mut self,
        q: Quote,
        policy: TriggerPolicy,
        mut activate: impl FnMut(OrderRequest) -> Result<OrderId, ActivationReject>,
    ) -> Result<Vec<ConditionalEvent>, &'static str> {
        q.validate()?;
        if self.last_sequence.is_some_and(|s| q.sequence <= s) || q.timestamp_ns < self.last_time_ns
        {
            return Err("conditional quote regressed");
        }
        let mut events = self.expire(q.timestamp_ns)?;
        self.last_sequence = Some(q.sequence);
        let selection = self.select(q, policy);
        for id in selection.ids {
            let Some(w) = self.remove(id) else {
                continue;
            }; // 可能已被先触发的OCO同伴撤销。
            match activate(w.request.order) {
                Ok(order_id) => {
                    self.metrics.triggered += 1;
                    events.push(ConditionalEvent::Triggered {
                        conditional_id: id,
                        order_id,
                        quote_sequence: q.sequence,
                    });
                    if let Some(g) = w.request.oco_group
                        && let Some(members) = self.groups.get(&g)
                    {
                        let ids: Vec<_> = members.iter().copied().collect();
                        for peer in ids {
                            events.push(self.cancel(peer, CancelReason::OcoPeerAccepted));
                        }
                    }
                }
                Err(reason) => {
                    self.metrics.activation_rejected += 1;
                    events.push(ConditionalEvent::ActivationRejected {
                        conditional_id: id,
                        reason,
                        quote_sequence: q.sequence,
                    });
                }
            }
        }
        Ok(events)
    }
    pub fn snapshot(&self) -> Option<ConditionalSnapshot> {
        // 从未使用的模块不改变旧配置的快照/报告字节，便于原始回执回归。
        (self.next_id != 1 || self.metrics != ConditionalMetrics::default()).then(|| {
            ConditionalSnapshot {
                next_id: self.next_id,
                pending: self.pending.values().copied().collect(),
                metrics: self.metrics,
                last_sequence: self.last_sequence,
                last_time_ns: self.last_time_ns,
            }
        })
    }
    pub fn restore(
        capacity: usize,
        s: ConditionalSnapshot,
        q: Option<Quote>,
        clock: u64,
    ) -> Result<Self, &'static str> {
        let mut b = Self::new(capacity)?;
        if s.pending.len() > capacity
            || s.next_id == 0
            || s.next_id > MAX_LIFETIME_ORDERS + 1
            || s.metrics.accepted != s.next_id - 1
            || s.metrics.rejected > MAX_LIFETIME_ORDERS * 64
            || s.last_time_ns > clock
            || s.last_sequence
                .is_some_and(|seq| q.is_none_or(|q| seq > q.sequence))
        {
            return Err("conditional snapshot identity/counter/clock invalid");
        }
        let resolved = [
            s.metrics.triggered,
            s.metrics.activation_rejected,
            s.metrics.cancelled,
            s.metrics.expired,
        ]
        .into_iter()
        .try_fold(s.pending.len() as u64, |a, n| a.checked_add(n))
        .ok_or("conditional counters overflow")?;
        if resolved != s.metrics.accepted {
            return Err("conditional lifecycle count mismatch");
        }
        let mut previous = 0;
        for w in s.pending {
            w.request.validate()?;
            if w.id.0 <= previous
                || w.id.0 >= s.next_id
                || w.submitted_at_ns > clock
                || w.request.expires_at_ns.is_some_and(|t| t <= clock)
                || q.is_none_or(|q| {
                    w.submitted_quote_sequence > q.sequence
                        || w.submitted_at_ns < q.timestamp_ns
                            && w.submitted_quote_sequence == q.sequence
                })
            {
                return Err("conditional waiting identity/time invalid");
            }
            previous = w.id.0;
            b.insert(w);
        }
        b.next_id = s.next_id;
        b.metrics = s.metrics;
        b.last_sequence = s.last_sequence;
        b.last_time_ns = s.last_time_ns;
        b.check_invariants()?;
        Ok(b)
    }
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        if self.len() > self.capacity {
            return Err("conditional capacity exceeded");
        }
        let mut p: [BTreeSet<(u64, ConditionalId)>; 4] = std::array::from_fn(|_| BTreeSet::new());
        let mut t = BTreeSet::new();
        let mut g: BTreeMap<u64, BTreeSet<ConditionalId>> = BTreeMap::new();
        for w in self.pending.values() {
            p[Self::index(w.request)].insert((w.request.trigger.units(), w.id));
            if let Some(e) = w.request.expires_at_ns {
                t.insert((e, w.id));
            }
            if let Some(group) = w.request.oco_group {
                g.entry(group).or_default().insert(w.id);
            }
        }
        if p != self.prices || t != self.expiry || g != self.groups {
            return Err("conditional index reconciliation failed");
        }
        Ok(())
    }
}
