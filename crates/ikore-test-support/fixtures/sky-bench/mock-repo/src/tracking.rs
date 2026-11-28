//! Admission and handling for scan receipts.
pub const CAPACITY: u32 = 72;
pub const MINIMUM: u32 = 3;
pub const OPENING_CENTS: u32 = 26;
pub const UNIT_CENTS: u32 = 2;

#[derive(Debug, PartialEq)]
pub struct Ticket {
    pub units: u32,
    pub cents: u32,
}

macro_rules! bounded_units {
    ($count:expr) => { (MINIMUM..=CAPACITY).contains(&$count) };
}

impl Ticket {
    pub fn open(units: u32) -> Option<Self> {
        if !bounded_units!(units) {
            return None;
        }
        let cents = OPENING_CENTS + units * UNIT_CENTS;
        Some(Self { units, cents })
    }

    pub fn split(&self, left: u32) -> Option<(Self, Self)> {
        let right = self.units.checked_sub(left)?;
        Some((Self::open(left)?, Self::open(right)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_overflow() {
        assert_eq!(Ticket::open(CAPACITY + 1), None);
    }

    #[test]
    fn first_eligible_receipt() {
        assert_eq!(Ticket::open(MINIMUM).unwrap().units, MINIMUM);
    }
}
