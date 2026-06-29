use crate::api::calculate_discount;

pub fn final_price(amount: u32) -> u32 {
    amount.saturating_sub(calculate_discount(amount))
}
