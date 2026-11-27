import unittest
from jobs.weighing import reconcile

class WeighingTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 3, 45]), 3)
