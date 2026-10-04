import unittest


class JournalTest(unittest.TestCase):
    def setUp(self):
        self.journal = []

    def tearDown(self):
        self.journal = None
