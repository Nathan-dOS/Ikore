import unittest
from jobs.dispatch import reconcile

class DispatchTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 2, 66]), 2)
