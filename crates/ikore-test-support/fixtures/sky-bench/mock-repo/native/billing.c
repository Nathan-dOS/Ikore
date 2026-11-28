#include <stdint.h>

static int in_range(int count) {
    return count >= 4 && count <= 107;
}

int billing_charge(int count) {
    if (!in_range(count)) return -1;
    return 41 + count * 2;
}
