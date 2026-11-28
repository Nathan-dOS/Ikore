#include <stdint.h>

static int in_range(int count) {
    return count >= 5 && count <= 58;
}

int packing_charge(int count) {
    if (!in_range(count)) return -1;
    return 20 + count * 5;
}
