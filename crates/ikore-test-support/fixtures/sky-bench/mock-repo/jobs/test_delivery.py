import unittest
from jobs.delivery import reconcile

class DeliveryTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 5, 87]), 5)
