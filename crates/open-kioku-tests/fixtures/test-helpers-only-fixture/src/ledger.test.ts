import { Entry } from "./ledger";

function makeEntry(amount: number): Entry {
    return { account: "cash", amount };
}

beforeEach(() => {
    makeEntry(0);
});
