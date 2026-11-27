#include <stdint.h>

static int in_range(int count) {
    return count >= 3 && count <= 72;
}

int tracking_charge(int count) {
    if (!in_range(count)) return -1;
    return 26 + count * 2;
}
