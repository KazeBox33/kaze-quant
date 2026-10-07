//! 热订单槽位：ID 不复用，物理槽位可复用。链按提交顺序连接而非物理下标。
use crate::types::{Order, OrderId};
use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    ops::Index,
};
const NONE: usize = usize::MAX;
struct Slot {
    order: Option<Order>,
    prev: usize,
    next: usize,
    active_prev: usize,
    active_next: usize,
}
pub(crate) struct OrderStore {
    slots: Vec<Slot>,
    free: Vec<usize>,
    ids: HashMap<OrderId, usize>,
    // 终结先后与提交先后不同；BTreeSet 确保回收的仍是最早提交的终态记录。
    terminal: BTreeSet<(u64, usize)>,
    head: usize,
    tail: usize,
    active_head: usize,
    active_tail: usize,
    active_len: usize,
}
impl OrderStore {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            ids: HashMap::new(),
            terminal: BTreeSet::new(),
            head: NONE,
            tail: NONE,
            active_head: NONE,
            active_tail: NONE,
            active_len: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn active_len(&self) -> usize {
        self.active_len
    }
    pub fn first(&self) -> Option<usize> {
        (self.head != NONE).then_some(self.head)
    }
    pub fn next(&self, i: usize) -> Option<usize> {
        (self.slots[i].next != NONE).then_some(self.slots[i].next)
    }
    pub fn first_active(&self) -> Option<usize> {
        (self.active_head != NONE).then_some(self.active_head)
    }
    pub fn next_active(&self, i: usize) -> Option<usize> {
        (self.slots[i].active_next != NONE).then_some(self.slots[i].active_next)
    }
    pub fn find(&self, id: OrderId) -> Option<usize> {
        self.ids.get(&id).copied()
    }
    pub fn get(&self, i: usize) -> &Order {
        self.slots[i].order.as_ref().expect("occupied order slot")
    }
    pub fn get_mut(&mut self, i: usize) -> &mut Order {
        self.slots[i].order.as_mut().expect("occupied order slot")
    }
    pub fn insert(&mut self, order: Order) {
        let id = order.id;
        let active = order.status.is_active();
        let i = self.free.pop().unwrap_or(self.slots.len());
        let s = Slot {
            order: Some(order),
            prev: self.tail,
            next: NONE,
            active_prev: if active { self.active_tail } else { NONE },
            active_next: NONE,
        };
        if i == self.slots.len() {
            self.slots.push(s);
        } else {
            self.slots[i] = s;
        }
        if self.tail == NONE {
            self.head = i;
        } else {
            self.slots[self.tail].next = i;
        }
        self.tail = i;
        assert!(
            self.ids.insert(id, i).is_none(),
            "duplicate internal order ID"
        );
        if active {
            if self.active_tail == NONE {
                self.active_head = i;
            } else {
                self.slots[self.active_tail].active_next = i;
            }
            self.active_tail = i;
            self.active_len += 1;
        } else {
            self.terminal.insert((id.0, i));
        }
    }
    // 只能在 Accepted/PartiallyFilled -> 终态的唯一转换处调用；不扫描、不换位。
    pub fn deactivate(&mut self, i: usize) {
        let (p, n) = (self.slots[i].active_prev, self.slots[i].active_next);
        if p == NONE {
            self.active_head = n;
        } else {
            self.slots[p].active_next = n;
        }
        if n == NONE {
            self.active_tail = p;
        } else {
            self.slots[n].active_prev = p;
        }
        self.slots[i].active_prev = NONE;
        self.slots[i].active_next = NONE;
        self.active_len -= 1;
        assert!(
            self.terminal.insert((self.get(i).id.0, i)),
            "duplicate terminal transition"
        );
    }
    pub fn compact(&mut self, budget: usize) -> usize {
        let count = self.terminal.len().saturating_sub(budget);
        for _ in 0..count {
            let (id, i) = self.terminal.pop_first().expect("terminal count");
            let (p, n) = (self.slots[i].prev, self.slots[i].next);
            if p == NONE {
                self.head = n;
            } else {
                self.slots[p].next = n;
            }
            if n == NONE {
                self.tail = p;
            } else {
                self.slots[n].prev = p;
            }
            self.ids.remove(&OrderId(id));
            self.slots[i].order = None;
            self.free.push(i);
        }
        count
    }
    pub fn view(&self) -> Orders<'_> {
        Orders(self)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        // 独立重建集合，避免错误链产生循环或隐藏被冻结订单。
        if self.ids.len() + self.free.len() != self.slots.len() {
            return Err("slot partition invalid");
        }
        let mut seen = vec![false; self.slots.len()];
        let mut prev = NONE;
        let mut last_id = 0;
        let mut cursor = self.first();
        while let Some(i) = cursor {
            if i >= self.slots.len() || seen[i] {
                return Err("history slot chain invalid");
            }
            seen[i] = true;
            let s = &self.slots[i];
            let o = s.order.as_ref().ok_or("free slot in history")?;
            if s.prev != prev || o.id.0 <= last_id || self.find(o.id) != Some(i) {
                return Err("history slot identity invalid");
            }
            prev = i;
            last_id = o.id.0;
            cursor = self.next(i);
        }
        if prev != self.tail || seen.iter().filter(|&&v| v).count() != self.len() {
            return Err("history slot coverage invalid");
        }
        for &i in &self.free {
            if i >= self.slots.len() || seen[i] || self.slots[i].order.is_some() {
                return Err("free slot invalid");
            }
            seen[i] = true;
        }
        let mut active = vec![false; self.slots.len()];
        prev = NONE;
        last_id = 0;
        cursor = self.first_active();
        while let Some(i) = cursor {
            if i >= self.slots.len() || active[i] {
                return Err("active slot chain invalid");
            }
            active[i] = true;
            let s = &self.slots[i];
            let o = s.order.as_ref().ok_or("free active slot")?;
            if s.active_prev != prev || !o.status.is_active() || o.id.0 <= last_id {
                return Err("active slot identity invalid");
            }
            prev = i;
            last_id = o.id.0;
            cursor = self.next_active(i);
        }
        if prev != self.active_tail || active.iter().filter(|&&v| v).count() != self.active_len {
            return Err("active slot count invalid");
        }
        let mut terminal = BTreeSet::new();
        for (i, s) in self.slots.iter().enumerate() {
            if let Some(o) = &s.order {
                if o.status.is_active() != active[i] {
                    return Err("active slot coverage invalid");
                }
                if !o.status.is_active() {
                    terminal.insert((o.id.0, i));
                }
            }
        }
        if terminal != self.terminal {
            return Err("terminal index invalid");
        }
        Ok(())
    }
}
/// 提交顺序的借用视图；无分配。随机序号访问 O(n)，ID 查询请用 Engine::order。
/// 不再承诺物理连续 slice；需要拥有数据时显式 to_vec()。
#[derive(Clone, Copy)]
pub struct Orders<'a>(&'a OrderStore);
impl<'a> Orders<'a> {
    pub fn len(self) -> usize {
        self.0.len()
    }
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub fn iter(self) -> OrdersIter<'a> {
        OrdersIter {
            store: self.0,
            next: self.0.first(),
            remaining: self.len(),
        }
    }
    pub fn to_vec(self) -> Vec<Order> {
        self.iter().cloned().collect()
    }
}
pub struct OrdersIter<'a> {
    store: &'a OrderStore,
    next: Option<usize>,
    remaining: usize,
}
impl<'a> Iterator for OrdersIter<'a> {
    type Item = &'a Order;
    fn next(&mut self) -> Option<Self::Item> {
        let i = self.next?;
        self.next = self.store.next(i);
        self.remaining -= 1;
        Some(self.store.get(i))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for OrdersIter<'_> {}
impl std::iter::FusedIterator for OrdersIter<'_> {}
impl fmt::Debug for Orders<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}
impl PartialEq for Orders<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl PartialEq<Vec<Order>> for Orders<'_> {
    fn eq(&self, other: &Vec<Order>) -> bool {
        self.iter().eq(other.iter())
    }
}
impl Index<usize> for Orders<'_> {
    type Output = Order;
    fn index(&self, i: usize) -> &Order {
        self.iter().nth(i).expect("order sequence out of bounds")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    fn order(id: u64, active: bool) -> Order {
        Order {
            id: OrderId(id),
            request: OrderRequest {
                side: Side::Buy,
                limit: Price::new(10).unwrap(),
                quantity: Quantity::new(1).unwrap(),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            remaining: 1,
            status: if active {
                OrderStatus::Accepted
            } else {
                OrderStatus::Rejected(RejectReason::NoMarket)
            },
            submitted_sequence: 1,
            eligible_at_ns: 1,
            reserved_per_unit: 10,
        }
    }
    #[test]
    fn slots_reuse_without_growing_lifetime_memory_or_reordering() {
        let mut s = OrderStore::new();
        s.insert(order(1, true));
        for id in 2..2002 {
            s.insert(order(id, false));
            assert_eq!(s.compact(0), 1);
            s.validate().unwrap();
            assert!(s.find(OrderId(id)).is_none());
        }
        assert_eq!(s.slots.len(), 2);
        s.insert(order(2002, true));
        assert_eq!(
            s.view().iter().map(|o| o.id.0).collect::<Vec<_>>(),
            [1, 2002]
        );
        s.validate().unwrap();
    }
    #[test]
    fn oldest_submission_evicted_even_when_it_terminates_last() {
        let mut s = OrderStore::new();
        s.insert(order(1, true));
        s.insert(order(2, false));
        s.insert(order(3, false));
        let i = s.find(OrderId(1)).unwrap();
        s.get_mut(i).status = OrderStatus::Cancelled;
        s.deactivate(i);
        assert_eq!(s.compact(2), 1);
        assert!(s.find(OrderId(1)).is_none());
        assert!(s.find(OrderId(2)).is_some());
        s.validate().unwrap();
    }
    #[test]
    fn corrupt_links_indices_and_free_lists_fail_independent_audit() {
        let mut s = OrderStore::new();
        s.insert(order(1, true));
        s.insert(order(2, true));
        s.slots[1].active_next = 0;
        assert!(s.validate().is_err());
        s.slots[1].active_next = 999;
        assert!(s.validate().is_err());
        s.slots[1].active_next = NONE;
        s.ids.insert(OrderId(1), 1);
        assert!(s.validate().is_err());
        s.ids.insert(OrderId(1), 0);
        s.slots[0].order.as_mut().unwrap().status = OrderStatus::Cancelled;
        assert!(s.validate().is_err());
        s.slots[0].order.as_mut().unwrap().status = OrderStatus::Accepted;
        s.validate().unwrap();
    }
}
