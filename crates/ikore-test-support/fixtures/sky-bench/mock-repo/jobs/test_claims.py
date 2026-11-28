import unittest
from jobs.claims import reconcile

class ClaimsTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 3, 101]), 3)
