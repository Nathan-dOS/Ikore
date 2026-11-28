import unittest
from jobs.sorting import reconcile

class SortingTests(unittest.TestCase):
    def test_export(self):
        self.assertEqual(reconcile([False, 4, 52]), 4)
