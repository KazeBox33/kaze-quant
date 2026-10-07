//! 单次突破入场与本地OCO退出教学策略。只跟踪自己的成交，不把手工仓位当策略仓位。
use crate::{
    conditional::*,
    engine::Engine,
    paper::PaperError,
    registry::CustomCheckpoint,
    strategy::{ActionBuffer, Strategy, StrategyView},
    types::*,
};
use serde::{Deserialize, Serialize};
const NAME: &str = "breakout-bracket";
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub entry_trigger: Price,
    pub entry_limit: Price,
    pub quantity: Quantity,
    pub stop_trigger: Price,
    pub stop_limit: Price,
    pub profit_trigger: Price,
    pub profit_limit: Price,
    pub entry_lifetime_ns: u64,
}
impl Parameters {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.stop_trigger >= self.entry_trigger
            || self.entry_trigger >= self.profit_trigger
            || self.entry_limit < self.entry_trigger
            || self.stop_limit > self.stop_trigger
            || self.profit_limit > self.profit_trigger
            || self.entry_lifetime_ns == 0
        {
            return Err("invalid bracket price hierarchy/lifetime");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Phase {
    New,
    EntryWaiting {
        id: Option<ConditionalId>,
        expires_at_ns: u64,
    },
    EntryWorking {
        id: OrderId,
        filled: u64,
        remaining: u64,
    },
    ArmExits {
        quantity: u64,
    },
    ExitsWaiting {
        quantity: u64,
        stop: Option<ConditionalId>,
        profit: Option<ConditionalId>,
        blocked: bool,
    },
    ExitWorking {
        id: OrderId,
        remaining: u64,
        limit: Price,
    },
    Complete,
    Paused {
        tracked_remaining: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BracketStrategy {
    parameters: Parameters,
    phase: Phase,
    group: u64,
}
impl BracketStrategy {
    pub fn new(parameters: Parameters) -> Result<Self, &'static str> {
        parameters.validate()?;
        Ok(Self {
            parameters,
            phase: Phase::New,
            group: 0,
        })
    }
    fn request(
        &self,
        entry: bool,
        profit: bool,
        qty: u64,
        expiry: Option<u64>,
    ) -> ConditionalRequest {
        let p = &self.parameters;
        ConditionalRequest {
            reference: if entry {
                Reference::Ask
            } else {
                Reference::Bid
            },
            direction: if entry || profit {
                Direction::AboveOrEqual
            } else {
                Direction::BelowOrEqual
            },
            trigger: if entry {
                p.entry_trigger
            } else if profit {
                p.profit_trigger
            } else {
                p.stop_trigger
            },
            order: OrderRequest {
                side: if entry { Side::Buy } else { Side::Sell },
                limit: if entry {
                    p.entry_limit
                } else if profit {
                    p.profit_limit
                } else {
                    p.stop_limit
                },
                quantity: Quantity::new(qty).expect("validated tracked quantity"),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            expires_at_ns: expiry,
            oco_group: if entry { None } else { Some(self.group) },
        }
    }
    fn validate_state(&self, p: &Parameters) -> Result<(), &'static str> {
        p.validate()?;
        if &self.parameters != p || self.group > crate::engine::MAX_LIFETIME_ORDERS {
            return Err("bracket parameters/group mismatch");
        }
        let valid_id = |id: u64| id > 0 && id <= crate::engine::MAX_LIFETIME_ORDERS;
        let qty = p.quantity.units();
        let valid = match self.phase {
            Phase::New => self.group == 0,
            Phase::EntryWaiting { id, expires_at_ns } => {
                self.group == 0 && id.is_some_and(|id| valid_id(id.0)) && expires_at_ns > 0
            }
            Phase::EntryWorking {
                id,
                filled,
                remaining,
            } => {
                self.group > 0
                    && valid_id(id.0)
                    && remaining > 0
                    && filled.checked_add(remaining) == Some(qty)
            }
            Phase::ArmExits { quantity } => self.group > 0 && quantity == qty,
            Phase::ExitsWaiting {
                quantity,
                stop,
                profit,
                blocked,
            } => {
                self.group > 0
                    && quantity == qty
                    && stop.is_none_or(|id| valid_id(id.0))
                    && profit.is_none_or(|id| valid_id(id.0))
                    && (blocked || stop.is_some() && profit.is_some())
                    && (stop.is_none() || stop != profit)
            }
            Phase::ExitWorking {
                id,
                remaining,
                limit,
            } => {
                self.group > 0
                    && valid_id(id.0)
                    && remaining > 0
                    && remaining <= qty
                    && [p.stop_limit, p.profit_limit].contains(&limit)
            }
            Phase::Complete => self.group > 0,
            Phase::Paused { tracked_remaining } => tracked_remaining <= qty,
        };
        if !valid {
            return Err("bracket phase invariant invalid");
        }
        Ok(())
    }
}
impl Strategy for BracketStrategy {
    fn on_quote_batch(&mut self, view: StrategyView<'_>, actions: &mut ActionBuffer) {
        match self.phase.clone() {
            Phase::New => {
                if view.active_orders != 0 {
                    return;
                }
                if view.account.position() != 0 {
                    self.phase = Phase::Paused {
                        tracked_remaining: 0,
                    };
                    return;
                }
                let Some(expiry) = view
                    .quote
                    .timestamp_ns
                    .checked_add(self.parameters.entry_lifetime_ns)
                else {
                    self.phase = Phase::Paused {
                        tracked_remaining: 0,
                    };
                    return;
                };
                let r = self.request(true, false, self.parameters.quantity.units(), Some(expiry));
                self.phase = Phase::EntryWaiting {
                    id: None,
                    expires_at_ns: expiry,
                };
                let _ = actions.push(Action::SubmitConditional(r));
            }
            Phase::ArmExits { quantity } => {
                if view.active_orders != 0
                    || view.account.position() != quantity
                    || view.account.reserved_sell() != 0
                {
                    self.phase = Phase::Paused {
                        tracked_remaining: quantity,
                    };
                    return;
                }
                self.phase = Phase::ExitsWaiting {
                    quantity,
                    stop: None,
                    profit: None,
                    blocked: false,
                };
                let _ = actions.push(Action::SubmitConditional(
                    self.request(false, false, quantity, None),
                ));
                let _ = actions.push(Action::SubmitConditional(
                    self.request(false, true, quantity, None),
                ));
            }
            Phase::ExitsWaiting {
                quantity,
                stop,
                profit,
                blocked: true,
            } => {
                self.phase = Phase::Paused {
                    tracked_remaining: quantity,
                };
                for id in [stop, profit].into_iter().flatten() {
                    let _ = actions.push(Action::CancelConditional(id));
                }
            }
            _ => (),
        }
    }
    fn on_conditional_event(&mut self, event: ConditionalEvent) {
        match (&mut self.phase, event) {
            (
                Phase::EntryWaiting { id, expires_at_ns },
                ConditionalEvent::Accepted {
                    conditional_id,
                    request,
                },
            ) if id.is_none()
                && request.order.side == Side::Buy
                && request.expires_at_ns == Some(*expires_at_ns) =>
            {
                *id = Some(conditional_id);
            }
            (
                Phase::EntryWaiting { id, .. },
                ConditionalEvent::Triggered {
                    conditional_id,
                    order_id,
                    ..
                },
            ) if *id == Some(conditional_id) => {
                self.group = conditional_id.0;
                self.phase = Phase::EntryWorking {
                    id: order_id,
                    filled: 0,
                    remaining: self.parameters.quantity.units(),
                };
            }
            (
                Phase::EntryWaiting { id, .. },
                ConditionalEvent::Cancelled { conditional_id, .. }
                | ConditionalEvent::ActivationRejected { conditional_id, .. },
            ) if *id == Some(conditional_id) => {
                self.phase = Phase::Paused {
                    tracked_remaining: 0,
                };
            }
            (Phase::EntryWaiting { .. }, ConditionalEvent::Rejected { request, .. })
                if request.order.side == Side::Buy =>
            {
                self.phase = Phase::Paused {
                    tracked_remaining: 0,
                };
            }
            (
                Phase::ExitsWaiting { stop, profit, .. },
                ConditionalEvent::Accepted {
                    conditional_id,
                    request,
                },
            ) if request.oco_group == Some(self.group) => match request.direction {
                Direction::BelowOrEqual if stop.is_none() => *stop = Some(conditional_id),
                Direction::AboveOrEqual if profit.is_none() => *profit = Some(conditional_id),
                _ => (),
            },
            (
                Phase::ExitsWaiting {
                    quantity,
                    stop,
                    profit,
                    ..
                },
                ConditionalEvent::Triggered {
                    conditional_id,
                    order_id,
                    ..
                },
            ) if [*stop, *profit].contains(&Some(conditional_id)) => {
                let limit = if *stop == Some(conditional_id) {
                    self.parameters.stop_limit
                } else {
                    self.parameters.profit_limit
                };
                self.phase = Phase::ExitWorking {
                    id: order_id,
                    remaining: *quantity,
                    limit,
                };
            }
            (
                Phase::ExitsWaiting {
                    stop,
                    profit,
                    blocked,
                    ..
                },
                ConditionalEvent::Cancelled { conditional_id, .. }
                | ConditionalEvent::ActivationRejected { conditional_id, .. },
            ) if [*stop, *profit].contains(&Some(conditional_id)) => {
                if *stop == Some(conditional_id) {
                    *stop = None;
                }
                if *profit == Some(conditional_id) {
                    *profit = None;
                }
                *blocked = true;
            }
            (Phase::ExitsWaiting { blocked, .. }, ConditionalEvent::Rejected { request, .. })
                if request.oco_group == Some(self.group) =>
            {
                *blocked = true;
            }
            _ => (),
        }
    }
    fn on_event(&mut self, event: Event) {
        match (&mut self.phase, event) {
            (
                Phase::EntryWorking {
                    id,
                    filled,
                    remaining,
                },
                Event::Fill(f),
            ) if *id == f.order_id => {
                *filled += f.quantity;
                *remaining -= f.quantity;
                if *remaining == 0 {
                    self.phase = Phase::ArmExits { quantity: *filled };
                }
            }
            (Phase::EntryWorking { id, filled, .. }, Event::Cancelled { order_id, .. })
                if *id == order_id =>
            {
                self.phase = Phase::Paused {
                    tracked_remaining: *filled,
                };
            }
            (Phase::ExitWorking { id, remaining, .. }, Event::Fill(f)) if *id == f.order_id => {
                *remaining -= f.quantity;
                if *remaining == 0 {
                    self.phase = Phase::Complete;
                }
            }
            (Phase::ExitWorking { id, remaining, .. }, Event::Cancelled { order_id, .. })
                if *id == order_id =>
            {
                self.phase = Phase::Paused {
                    tracked_remaining: *remaining,
                };
            }
            _ => (),
        }
    }
    fn validate_execution(&self, e: &Engine) -> Result<(), &'static str> {
        self.validate_state(&self.parameters)?;
        let child = match self.phase {
            Phase::EntryWorking { id, remaining, .. } => {
                Some((id, remaining, Side::Buy, self.parameters.entry_limit))
            }
            Phase::ExitWorking {
                id,
                remaining,
                limit,
            } => Some((id, remaining, Side::Sell, limit)),
            _ => None,
        };
        if let Some((id, remaining, side, limit)) = child {
            let o = e.order(id).ok_or("bracket child absent")?;
            let expected = OrderRequest {
                side,
                limit,
                quantity: self.parameters.quantity,
                time_in_force: TimeInForce::GoodTilCancelled,
            };
            if !o.status.is_active() || o.remaining != remaining || o.request != expected {
                return Err("bracket child mismatch");
            }
        }
        Ok(())
    }
    fn validate_conditionals(&self, book: &ConditionalBook) -> Result<(), &'static str> {
        let mut expected = Vec::new();
        match self.phase {
            Phase::EntryWaiting {
                id: Some(id),
                expires_at_ns,
            } => {
                let waiting = book.get(id).ok_or("bracket entry condition absent")?;
                if waiting
                    .submitted_at_ns
                    .checked_add(self.parameters.entry_lifetime_ns)
                    != Some(expires_at_ns)
                {
                    return Err("bracket entry lifetime mismatch");
                }
                expected.push((
                    id,
                    self.request(
                        true,
                        false,
                        self.parameters.quantity.units(),
                        Some(expires_at_ns),
                    ),
                ));
            }
            Phase::ExitsWaiting {
                quantity,
                stop,
                profit,
                ..
            } => {
                if let Some(id) = stop {
                    expected.push((id, self.request(false, false, quantity, None)));
                }
                if let Some(id) = profit {
                    expected.push((id, self.request(false, true, quantity, None)));
                }
            }
            _ => (),
        }
        for (id, r) in expected {
            if book.get(id).is_none_or(|w| w.request != r) {
                return Err("bracket conditional identity/request mismatch");
            }
        }
        Ok(())
    }
    fn diagnostics(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"strategy":NAME,"phase":self.phase,"oco_group":self.group,"scope":"single-cycle paper breakout; fixed stop-limit exits; no guaranteed fill or alpha"}),
        )
    }
    fn custom_checkpoint(&self) -> Option<CustomCheckpoint> {
        Some(CustomCheckpoint {
            name: NAME.into(),
            version: 1,
            state: serde_json::to_value(self).expect("bounded bracket state"),
        })
    }
}
pub fn factory(
    parameters: &serde_json::Value,
    state: Option<&serde_json::Value>,
) -> Result<Box<dyn Strategy>, PaperError> {
    let p: Parameters = serde_json::from_value(parameters.clone())?;
    let s = if let Some(v) = state {
        let s: BracketStrategy = serde_json::from_value(v.clone())?;
        s.validate_state(&p)?;
        s
    } else {
        BracketStrategy::new(p)?
    };
    Ok(Box::new(s))
}
