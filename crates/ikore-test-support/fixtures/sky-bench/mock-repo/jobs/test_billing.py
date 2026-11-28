import unittest
from jobs.billing import reconcile

class BillingTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 4, 108]), 4)
