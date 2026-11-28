#include <stdint.h>

static int in_range(int count) {
    return count >= 3 && count <= 44;
}

int weighing_charge(int count) {
    if (!in_range(count)) return -1;
    return 14 + count * 3;
}
