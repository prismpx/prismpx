/*
 * prismpx C API
 *
 * Contract:
 *  - Handles are opaque. Free what you create: prismpx_options_free,
 *    prismpx_result_free.
 *  - Functions returning int return PRISMPX_OK (0) or a negative error code;
 *    prismpx_last_error() then describes the failure (thread-local).
 *  - Passing NULL options to prismpx_optimize uses the defaults
 *    (lossless, default effort, safe metadata policy, verification on).
 *  - ABI 2 added prismpx_options_set_mode and prismpx_options_set_quality.
 *  - Fields may be added in later versions through new setter functions;
 *    check prismpx_abi_version() if you depend on newer ones.
 */
#ifndef PRISMPX_H
#define PRISMPX_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PRISMPX_ABI_VERSION 2

#define PRISMPX_OK 0
#define PRISMPX_ERR_NULL_POINTER -1
#define PRISMPX_ERR_INVALID_ARGUMENT -2
#define PRISMPX_ERR_UNSUPPORTED_FORMAT -3
#define PRISMPX_ERR_UNSUPPORTED -4
#define PRISMPX_ERR_MALFORMED -5
#define PRISMPX_ERR_VERIFICATION_FAILED -6
#define PRISMPX_ERR_PANIC -99

#define PRISMPX_EFFORT_FAST 0
#define PRISMPX_EFFORT_DEFAULT 1
#define PRISMPX_EFFORT_MAX 2

#define PRISMPX_MODE_LOSSLESS 0
#define PRISMPX_MODE_VISUALLY_LOSSLESS 1

#define PRISMPX_METADATA_SAFE 0
#define PRISMPX_METADATA_KEEP_ALL 1
#define PRISMPX_METADATA_COLOR_ONLY 2

typedef struct PrismpxOptions PrismpxOptions;
typedef struct PrismpxResult PrismpxResult;

uint32_t prismpx_abi_version(void);
const char *prismpx_version(void);
const char *prismpx_last_error(void);

PrismpxOptions *prismpx_options_new(void);
void prismpx_options_free(PrismpxOptions *opts);
int prismpx_options_set_effort(PrismpxOptions *opts, int effort);
int prismpx_options_set_metadata(PrismpxOptions *opts, int policy);
int prismpx_options_set_verify(PrismpxOptions *opts, int verify);
int prismpx_options_set_clean_transparent(PrismpxOptions *opts, int clean);
/* Since ABI 2. */
int prismpx_options_set_mode(PrismpxOptions *opts, int mode);
/* Minimum SSIMULACRA2 score (0..100) in visually-lossless mode; default 85.
 * Since ABI 2. */
int prismpx_options_set_quality(PrismpxOptions *opts, float quality);

int prismpx_optimize(const uint8_t *input, size_t input_len,
                     const PrismpxOptions *opts, PrismpxResult **out);

const uint8_t *prismpx_result_data(const PrismpxResult *res, size_t *len);
size_t prismpx_result_original_size(const PrismpxResult *res);
int prismpx_result_optimized(const PrismpxResult *res);
const char *prismpx_result_strategy(const PrismpxResult *res);
void prismpx_result_free(PrismpxResult *res);

#ifdef __cplusplus
}
#endif

#endif /* PRISMPX_H */
