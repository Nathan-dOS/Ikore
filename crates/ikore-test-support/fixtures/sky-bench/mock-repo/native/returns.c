#include <stdint.h>

static int in_range(int count) {
    return count >= 2 && count <= 93;
}

int returns_charge(int count) {
    if (!in_range(count)) return -1;
    return 35 + count * 5;
}
