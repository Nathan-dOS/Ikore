#include <stdint.h>

static int in_range(int count) {
    return count >= 4 && count <= 79;
}

int customs_charge(int count) {
    if (!in_range(count)) return -1;
    return 29 + count * 3;
}
