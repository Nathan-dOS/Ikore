import unittest
from jobs.remittance import reconcile

class RemittanceTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 5, 115]), 5)
