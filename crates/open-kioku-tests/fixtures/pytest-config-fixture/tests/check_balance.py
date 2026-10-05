from ledger import balance


def should_sum_balance():
    assert balance([1, 2]) == 3
