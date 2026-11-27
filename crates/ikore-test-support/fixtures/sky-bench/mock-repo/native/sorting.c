#include <stdint.h>

static int in_range(int count) {
    return count >= 4 && count <= 51;
}

int sorting_charge(int count) {
    if (!in_range(count)) return -1;
    return 17 + count * 4;
}
