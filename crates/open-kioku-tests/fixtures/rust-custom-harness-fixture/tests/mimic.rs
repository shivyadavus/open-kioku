use libtest_mimic::{Arguments, Trial};
use ledger::post_entry;

fn main() {
    let args = Arguments::from_args();
    let trials = vec![Trial::test("posts_entry", check_posts_entry)];
    libtest_mimic::run(&args, trials).exit();
}

fn check_posts_entry() -> Result<(), libtest_mimic::Failed> {
    assert_eq!(post_entry(&mut vec![], 1), 1);
    Ok(())
}
