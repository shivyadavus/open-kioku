pub fn post_entry(ledger: &mut Vec<i64>, amount: i64) -> i64 {
    ledger.push(amount);
    ledger.iter().sum()
}

pub fn rounds_half_up(value: f64) -> i64 {
    (value + 0.5) as i64
}
