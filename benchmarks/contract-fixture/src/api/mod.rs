pub fn calculate_discount(amount: u32) -> u32 {
    amount / 10
}

pub fn format_receipt(amount: u32) -> String {
    format!("receipt:{amount}")
}
