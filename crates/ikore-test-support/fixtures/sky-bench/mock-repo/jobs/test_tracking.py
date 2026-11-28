import unittest
from jobs.tracking import reconcile

class TrackingTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 3, 73]), 3)
