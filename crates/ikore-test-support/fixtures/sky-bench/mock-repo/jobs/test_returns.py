import unittest
from jobs.returns import reconcile

class ReturnsTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 2, 94]), 2)
