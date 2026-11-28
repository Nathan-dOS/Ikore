import unittest
from jobs.customs import reconcile

class CustomsTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 4, 80]), 4)
