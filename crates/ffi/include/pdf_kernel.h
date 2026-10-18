/* pdf-kernel C API.
 *
 * Every operation goes through pk_engine_call() with the JSON protocol
 * described in docs/PROTOCOL.md. Strings are NUL-terminated UTF-8.
 * Strings returned by the library must be freed with pk_string_free().
 */
#ifndef PDF_KERNEL_H
#define PDF_KERNEL_H

#ifdef __cplusplus
extern "C" {
#endif

typedef struct PkEngine PkEngine;

/* config_json: NULL to read env/.env, or a JSON Config object.
 * Returns NULL on failure and, if error_out is non-NULL, stores a message
 * in *error_out (free it with pk_string_free). */
PkEngine *pk_engine_new(const char *config_json, char **error_out);

/* Blocking; call off the UI thread. Thread-safe. Returns a JSON response. */
char *pk_engine_call(const PkEngine *engine, const char *request_json);

void pk_string_free(char *s);

void pk_engine_free(PkEngine *engine);

/* Static string; do not free. */
const char *pk_version(void);

#ifdef __cplusplus
}
#endif

#endif /* PDF_KERNEL_H */
