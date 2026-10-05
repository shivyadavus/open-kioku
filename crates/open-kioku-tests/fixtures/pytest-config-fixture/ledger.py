def post_entry(ledger, amount):
    ledger.append(amount)
    return sum(ledger)


def balance(ledger):
    return sum(ledger)
