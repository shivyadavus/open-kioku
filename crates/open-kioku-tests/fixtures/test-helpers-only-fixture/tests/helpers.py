def make_entry(amount):
    return {"account": "cash", "amount": amount}


def test_data():
    return [make_entry(1), make_entry(2)]
