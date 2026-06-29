use crate::api::format_receipt;

pub fn checkout_summary(amount: u32) -> String {
    format_receipt(amount)
}
