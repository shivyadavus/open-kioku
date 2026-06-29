use contract_fixture::domain::pricing::final_price;

#[test]
fn pricing_discount_contract() {
    assert_eq!(final_price(100), 90);
}
