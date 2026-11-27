#include <stdint.h>

static int in_range(int count) {
    return count >= 5 && count <= 114;
}

int remittance_charge(int count) {
    if (!in_range(count)) return -1;
    return 44 + count * 3;
}
