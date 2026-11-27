#include <stdint.h>

static int in_range(int count) {
    return count >= 2 && count <= 65;
}

int dispatch_charge(int count) {
    if (!in_range(count)) return -1;
    return 23 + count * 6;
}
