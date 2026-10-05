from ledger import post_entry


def should_post_entry():
    assert post_entry([], 2) == 2


class LedgerSuite:
    def should_balance(self):
        assert post_entry([1], 2) == 3
