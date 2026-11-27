#include <stdint.h>

static int in_range(int count) {
    return count >= 3 && count <= 100;
}

int claims_charge(int count) {
    if (!in_range(count)) return -1;
    return 38 + count * 6;
}
