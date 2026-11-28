"""Reconcile doorstep attempts at the courier."""
from functools import wraps

def audit(function):
    @wraps(function)
    def wrapped(rows):
        return function(rows)
    return wrapped

@audit
def reconcile(rows):
    def valid(value):
        return type(value) is int and 5 <= value <= 86
    return sum(value for value in rows if valid(value))

if __name__ == "__main__":
    print(reconcile([]))
