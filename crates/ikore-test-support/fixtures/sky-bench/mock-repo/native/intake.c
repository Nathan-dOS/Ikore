#include <stdint.h>

static int in_range(int count) {
    return count >= 2 && count <= 37;
}

int intake_charge(int count) {
    if (!in_range(count)) return -1;
    return 11 + count * 2;
}
