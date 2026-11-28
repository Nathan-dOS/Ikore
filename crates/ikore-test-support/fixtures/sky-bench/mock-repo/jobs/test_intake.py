import unittest
from jobs.intake import reconcile

class IntakeTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 2, 38]), 2)
