use togi_machine::{CoreId, CoreSet, PER_CORE_MAX, PerCore, TargetOffset};

use crate::constraints::{Combination, FailurePoint};
use crate::{Depth, Profile, Ranking};

type Depths = PerCore<i32>;
type Needs = PerCore<Option<i32>>;

/// Every core ends at its floor or at a count that breaks some combination still reached, so the
/// search branches only over which member breaks each such combination.
pub(crate) fn solve(
    cores: CoreSet,
    failure_points: &PerCore<FailurePoint>,
    combinations: &[Combination],
    hi: &Profile,
    ranking: &Ranking,
) -> Option<Profile> {
    let floor_limit = TargetOffset::clamped(i64::MIN).signed();
    let mut p = Depths::default();
    let mut top = Depths::default();
    let mut total = 0;
    for core in cores {
        let floor = failure_points.get(core).map_or(floor_limit, |point| {
            (point.offset().signed() + 1).max(floor_limit)
        });
        let ceiling = hi.get(core).map_or(floor_limit, Depth::signed);
        if floor > ceiling {
            return None;
        }
        p.set(core, floor);
        top.set(core, ceiling);
        total += floor;
    }
    let needs = break_thresholds(combinations, &p, &top)?;
    let mut search = Search {
        needs,
        order: tie_order(cores, ranking),
        p,
        total,
        best: None,
    };
    search.visit();
    search.best.map(|(best, _)| {
        Profile::over(cores, |core| {
            TargetOffset::clamped(i64::from(best.get(core).unwrap_or_default()))
        })
    })
}

/// For each combination the floor profile `p` reaches, the offset at or above which each core
/// breaks it, `None` where that offset is above `top`. Drops each combination another one
/// implies, and returns `None` when some combination cannot be broken within `top`.
fn break_thresholds(combinations: &[Combination], p: &Depths, top: &Depths) -> Option<Vec<Needs>> {
    let mut needs: Vec<Needs> = Vec::new();
    for combination in combinations {
        let mut need = Needs::default();
        let mut reached = true;
        let mut breakable = false;
        for (member, offset) in combination.members().iter() {
            let at = offset.signed() + 1;
            if p.get(member).unwrap_or_default() > offset.signed() {
                reached = false;
                break;
            }
            if at <= top.get(member).unwrap_or_default() {
                need.set(member, Some(at));
                breakable = true;
            }
        }
        if !reached {
            continue;
        }
        if !breakable {
            return None;
        }
        needs.push(need);
    }
    let kept = needs
        .iter()
        .enumerate()
        .filter(|&(k, b)| {
            !needs
                .iter()
                .enumerate()
                .any(|(j, a)| j != k && implies(a, b) && (j < k || !implies(b, a)))
        })
        .map(|(_, need)| *need)
        .collect();
    Some(kept)
}

/// Whether breaking `b` already breaks `a`: wherever `a` can be broken, `b` breaks at no deeper an
/// offset.
fn implies(a: &Needs, b: &Needs) -> bool {
    a.iter()
        .all(|(core, at)| match (at, b.get(core).flatten()) {
            (Some(a), Some(b)) => b <= a,
            (Some(_), None) => false,
            (None, _) => true,
        })
}

fn tie_order(cores: CoreSet, ranking: &Ranking) -> Vec<CoreId> {
    let mut placed = CoreSet::default();
    let mut order = Vec::with_capacity(cores.len());
    for &core in ranking.order() {
        if cores.contains(core) && placed.insert(core) {
            order.push(core);
        }
    }
    order.extend(cores.iter().filter(|&core| !placed.contains(core)));
    order
}

#[cfg(test)]
thread_local! {
    /// Search nodes visited on this thread since the last `take_nodes`.
    static NODES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The search nodes visited on this thread since the previous call, for work-bound tests.
#[cfg(test)]
pub(crate) fn take_nodes() -> u64 {
    NODES.with(|nodes| nodes.replace(0))
}

struct Search {
    needs: Vec<Needs>,
    order: Vec<CoreId>,
    p: Depths,
    total: i32,
    best: Option<(Depths, i32)>,
}

impl Search {
    fn visit(&mut self) {
        #[cfg(test)]
        NODES.with(|nodes| nodes.set(nodes.get() + 1));
        let mut branch = None;
        let mut branch_moves = 0;
        let mut bound = 0;
        for (k, need) in self.needs.iter().enumerate() {
            let mut cheapest = i32::MAX;
            let mut count = 0;
            for (core, at) in need.iter() {
                let Some(at) = at else { continue };
                let current = self.p.get(core).unwrap_or_default();
                if current >= at {
                    count = 0;
                    break;
                }
                cheapest = cheapest.min(at - current);
                count += 1;
            }
            if count == 0 {
                continue;
            }
            bound = bound.max(cheapest);
            if branch.is_none() || count < branch_moves {
                branch = Some(k);
                branch_moves = count;
            }
        }
        if let Some((_, best_total)) = self.best {
            if self.total + bound > best_total {
                return;
            }
        }
        let Some(branch) = branch else {
            self.record();
            return;
        };
        let need = self.needs[branch];
        let mut options = [(CoreId::FIRST, 0); PER_CORE_MAX];
        let mut len = 0;
        for &core in &self.order {
            if let Some(at) = need.get(core).flatten() {
                options[len] = (core, at);
                len += 1;
            }
        }
        let p = self.p;
        options[..len].sort_by_key(|&(core, at)| at - p.get(core).unwrap_or_default());
        for &(core, at) in &options[..len] {
            let prior = self.p.get(core).unwrap_or_default();
            self.p.set(core, at);
            self.total += at - prior;
            self.visit();
            self.total -= at - prior;
            self.p.set(core, prior);
        }
    }

    fn record(&mut self) {
        let better = match &self.best {
            None => true,
            Some((best, best_total)) => {
                self.total < *best_total || (self.total == *best_total && self.deeper_on_tie(best))
            }
        };
        if better {
            self.best = Some((self.p, self.total));
        }
    }

    fn deeper_on_tie(&self, best: &Depths) -> bool {
        self.order
            .iter()
            .copied()
            .find(|&core| self.p.get(core) != best.get(core))
            .is_some_and(|core| self.p.get(core) < best.get(core))
    }
}
