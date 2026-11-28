import unittest
from jobs.packing import reconcile

class PackingTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 5, 59]), 5)
