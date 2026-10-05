import unittest

import pytest

from pricing import discount


@pytest.fixture
def rate():
    return 0.25


def make_price():
    return 8.0


def test_discount_by_rate(rate):
    assert discount(make_price(), rate) == 6.0


class DiscountTest(unittest.TestCase):
    def setUp(self):
        self.price = make_price()

    def tearDown(self):
        self.price = None

    def test_zero_rate_keeps_price(self):
        self.assertEqual(discount(self.price, 0), 8.0)

    def assert_discounted(self, rate, expected):
        self.assertEqual(discount(self.price, rate), expected)
