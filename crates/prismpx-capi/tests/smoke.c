/* Smoke test for the C ABI. Usage: smoke <input.png> <output.png> */
#include <stdio.h>
#include <stdlib.h>
#include "prismpx.h"

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <input.png> <output.png>\n", argv[0]);
        return 2;
    }
    if (prismpx_abi_version() != PRISMPX_ABI_VERSION) {
        fprintf(stderr, "ABI mismatch\n");
        return 1;
    }

    FILE *f = fopen(argv[1], "rb");
    if (!f) { perror("open"); return 1; }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc((size_t)n);
    if (fread(buf, 1, (size_t)n, f) != (size_t)n) { perror("read"); return 1; }
    fclose(f);

    PrismpxOptions *opts = prismpx_options_new();
    if (prismpx_options_set_effort(opts, 7) != PRISMPX_ERR_INVALID_ARGUMENT) return 1;
    prismpx_options_set_effort(opts, PRISMPX_EFFORT_MAX);
    if (argc > 3 || getenv("PRISMPX_LOSSY")) {
        if (prismpx_options_set_mode(opts, PRISMPX_MODE_VISUALLY_LOSSLESS) != PRISMPX_OK) return 1;
        if (prismpx_options_set_quality(opts, 120.0f) != PRISMPX_ERR_INVALID_ARGUMENT) return 1;
        prismpx_options_set_quality(opts, 85.0f);
    }

    PrismpxResult *res = NULL;
    uint8_t junk[4] = {1, 2, 3, 4};
    if (prismpx_optimize(junk, 4, opts, &res) != PRISMPX_ERR_UNSUPPORTED_FORMAT || res != NULL) return 1;
    printf("expected error: %s\n", prismpx_last_error());

    int rc = prismpx_optimize(buf, (size_t)n, opts, &res);
    if (rc != PRISMPX_OK) {
        fprintf(stderr, "error %d: %s\n", rc, prismpx_last_error());
        return 1;
    }
    size_t len = 0;
    const uint8_t *data = prismpx_result_data(res, &len);
    printf("prismpx %s: %zu -> %zu bytes (optimized=%d)\n  %s\n", prismpx_version(),
           prismpx_result_original_size(res), len, prismpx_result_optimized(res),
           prismpx_result_strategy(res));

    FILE *o = fopen(argv[2], "wb");
    fwrite(data, 1, len, o);
    fclose(o);

    prismpx_result_free(res);
    prismpx_options_free(opts);
    free(buf);
    return 0;
}
