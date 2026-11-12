/*
 * tpt_search.h: C interface to tpt-search.
 *
 * Hand-written to match crates/tpt-search-capi/src/lib.rs. Documents and results are
 * UTF-8 JSON, in the same shapes as the HTTP API.
 *
 * Every function returns a TPT_* status code. On failure, tpt_last_error() describes the
 * problem for the calling thread. Strings returned through an out parameter belong to the
 * caller and must be released with tpt_string_free().
 */

#ifndef TPT_SEARCH_H
#define TPT_SEARCH_H

#include <stdbool.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Status codes. */
#define TPT_OK                    0  /* success */
#define TPT_ERR_INVALID_ARGUMENT  1  /* null pointer, or a string that is not UTF-8 */
#define TPT_ERR_NOT_FOUND         2  /* the index or document does not exist */
#define TPT_ERR_INVALID_INPUT     3  /* the schema, document or query was rejected */
#define TPT_ERR_CONFLICT          4  /* an index with that name already exists */
#define TPT_ERR_INTERNAL          5  /* internal error, such as a caught panic */
#define TPT_ERR_TIMEOUT           6  /* the query exceeded its time budget */

/* Largest accepted `limit` for tpt_search(). */
#define TPT_MAX_LIMIT 1000

/* Opaque engine handle. */
typedef struct TptEngine TptEngine;

/* Creates an engine with no indexes. Release it with tpt_engine_free(). Never returns NULL
 * unless memory is exhausted. */
TptEngine *tpt_engine_new(void);

/* Releases an engine and every index it holds. NULL is accepted and does nothing. */
void tpt_engine_free(TptEngine *engine);

/* Creates an index from a JSON schema, as the HTTP API accepts it. */
int tpt_create_index(const TptEngine *engine, const char *name, const char *schema_json);

/* Removes an index and its graph. TPT_ERR_NOT_FOUND if it does not exist. */
int tpt_drop_index(const TptEngine *engine, const char *name);

/* Inserts or replaces a document. body_json is a JSON object of field values. */
int tpt_upsert(const TptEngine *engine, const char *index, const char *id,
               const char *body_json);

/* Deletes a document. If `existed` is not NULL it receives whether the document was present. */
int tpt_delete(const TptEngine *engine, const char *index, const char *id, bool *existed);

/* Reads one document as JSON. On success *out_json receives a string to free with
 * tpt_string_free(). */
int tpt_get(const TptEngine *engine, const char *index, const char *id, char **out_json);

/* Runs a query. The JSON result is {"total_hits", "took_ms", "hits": [...]}, where each hit
 * has "id", "document" and "highlights". limit 0 selects the default; values above
 * TPT_MAX_LIMIT are rejected. On success *out_json receives a string to free with
 * tpt_string_free(). */
int tpt_search(const TptEngine *engine, const char *index, const char *query, size_t limit,
               char **out_json);

/* Releases a string returned through an out parameter. NULL is accepted and does nothing. */
void tpt_string_free(char *text);

/* The message for the most recent failed call on this thread. The pointer is valid until the
 * next call on this thread. It is the empty string after a successful call. */
const char *tpt_last_error(void);

/* The library version, for example "0.1.0". The pointer is static. */
const char *tpt_version(void);

#ifdef __cplusplus
}
#endif

#endif /* TPT_SEARCH_H */
