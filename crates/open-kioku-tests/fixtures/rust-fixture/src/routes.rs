#[get("/v1/orders")]
pub async fn orders_route() {}

pub async fn call_orders_route() {
    let _ = reqwest::get("https://example.com/v1/orders").await;
}
