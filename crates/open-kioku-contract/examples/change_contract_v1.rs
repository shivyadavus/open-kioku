use open_kioku_contract::{schema, ChangeContractV1};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let contract: ChangeContractV1 =
        serde_json::from_str(include_str!("../tests/fixtures/change_contract_v1.json"))?;
    contract.validate()?;

    println!("{}", serde_json::to_string_pretty(&contract)?);
    println!("{}", serde_json::to_string_pretty(&schema())?);
    Ok(())
}
