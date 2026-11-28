//! Executed physical constitutive surfaces. A signed local polynomial is a
//! localization hint, not an event or an instruction to commit trial history.
use super::arithmetic::{Scalar, Series};
use crate::{Error, Result};
use std::cmp::Ordering;

/// Bound device-local sites independently of Taylor coefficient storage.
const MAX_GUARDS: usize = 256;

/// Strict predicate used by the constitutive law. Equality is on the
/// non-selected side; a nonconstant exact-zero germ uses its first nonzero
/// coefficient to determine the selected one-sided branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuardPredicate {
    Less,
    Greater,
}

impl GuardPredicate {
    fn selected(self, order: Option<Ordering>) -> bool {
        order
            == Some(match self {
                Self::Less => Ordering::Less,
                Self::Greater => Ordering::Greater,
            })
    }
}

/// One executed branch in the producer's immutable family-local site space.
/// The surface is lhs-rhs in normalized time, with the selected predicate.
#[derive(Clone, Copy, Debug)]
pub struct Guard {
    pub site: u16,
    pub predicate: GuardPredicate,
    pub selected: bool,
    pub surface: Series,
}

/// Caller-owned bounded trace, reset by each checked Taylor evaluation.
/// Records are in execution order; inactive or unexecuted sites are absent.
#[derive(Debug, Default)]
pub struct GuardTrace {
    guards: Vec<Guard>,
    error: Option<Error>,
}

impl GuardTrace {
    /// Executed sites from the last successful evaluation.
    #[must_use = "executed guards determine physical branch localization"]
    pub fn records(&self) -> &[Guard] {
        &self.guards
    }

    pub(crate) fn reset(&mut self) {
        self.guards.clear();
        self.error = None;
    }

    pub(super) fn validate(&self, order: usize) -> Result<()> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if self.guards.iter().any(|guard| {
            guard.surface.order() > order
                || !guard.surface.is_finite()
                || guard.predicate.selected(guard.surface.compare(0.)) != guard.selected
        }) {
            return Err(Error::Domain("inconsistent physical guard germ".into()));
        }
        Ok(())
    }

    fn push(&mut self, guard: &Guard) {
        if self.error.is_some() {
            return;
        }
        if self.guards.len() == MAX_GUARDS {
            self.error = Some(Error::Shape("physical guard site limit"));
        } else if self.guards.iter().any(|other| other.site == guard.site) {
            self.error = Some(Error::Schema("duplicate physical guard site"));
        } else if self.guards.try_reserve(1).is_err() {
            self.error = Some(Error::Allocation);
        } else {
            self.guards.push(*guard);
        }
    }
}

/// Select the actual constitutive branch and optionally retain its surface.
/// Scalar evaluation uses a disabled recorder, with no allocation or trace.
pub struct GuardRecorder<'a>(Option<&'a mut GuardTrace>);

impl<'a> GuardRecorder<'a> {
    #[must_use = "the recorder selects constitutive branches without retaining a trace"]
    pub const fn disabled() -> Self {
        Self(None)
    }

    pub const fn new(trace: &'a mut GuardTrace) -> Self {
        Self(Some(trace))
    }

    /// Compare the actual operands, retaining the exact signed Taylor surface
    /// only when the model parameters make this branch physically relevant.
    pub fn select<S: Scalar>(
        &mut self,
        site: u16,
        predicate: GuardPredicate,
        lhs: S,
        rhs: S,
        active: bool,
    ) -> bool {
        let selected = predicate.selected(lhs.compare_to(rhs));
        if active && let Some(trace) = self.0.as_deref_mut() {
            trace.push(&Guard {
                site,
                predicate,
                selected,
                surface: (lhs - rhs).into_series(),
            });
        }
        selected
    }
}
