/* OSqlite — SQLite configuration for bare-metal kernel.
 *
 * Included by build.rs via -DSQLITE_CUSTOM_INCLUDE=... or
 * -include sqlite_config.h.
 *
 * Goal: strip SQLite down to the bare minimum for a single-process,
 * mono-threaded, no-libc environment with our own VFS and allocator.
 */

#ifndef HEAVEN_SQLITE_CONFIG_H
#define HEAVEN_SQLITE_CONFIG_H

/* ----- OS / environment ----- */
#define SQLITE_OS_OTHER 1           /* No default VFS — we register our own */
#define SQLITE_THREADSAFE 0         /* Single-threaded (no mutexes) */

/* ----- Memory allocator ----- */
#define SQLITE_ZERO_MALLOC 1        /* We provide sqlite3_malloc/free/realloc */

/* ----- Feature trimming ----- */
#define SQLITE_OMIT_WAL 1           /* Simplifies VFS (no shared memory needed yet) */
#define SQLITE_OMIT_LOAD_EXTENSION 1
#define SQLITE_OMIT_PROGRESS_CALLBACK 1
#define SQLITE_OMIT_COMPLETE 1
#define SQLITE_OMIT_TCL_VARIABLE 1
#define SQLITE_OMIT_UTF16 1
#define SQLITE_OMIT_DEPRECATED 1
#define SQLITE_OMIT_SHARED_CACHE 1
#define SQLITE_OMIT_AUTOINIT 1      /* We call sqlite3_initialize() ourselves */
#define SQLITE_OMIT_DECLTYPE 1
#define SQLITE_OMIT_TRACE 1
#define SQLITE_OMIT_GET_TABLE 1     /* We use sqlite3_exec with callback */
#define SQLITE_OMIT_AUTHORIZATION 1

/* ----- Performance / safety ----- */
#define SQLITE_DEFAULT_MEMSTATUS 0  /* No memory usage tracking */
#define SQLITE_DQS 0               /* Double-quoted strings are errors */
#define SQLITE_LIKE_DOESNT_MATCH_BLOBS 1
#define SQLITE_MAX_EXPR_DEPTH 0     /* No limit (uses less stack checking) */
#define SQLITE_DEFAULT_FOREIGN_KEYS 1

/* ----- Disable floating-point if not needed ----- */
/* We keep floats enabled — SQLite REAL type needs them, and our kernel
 * runs with SSE enabled (Limine sets up SSE/AVX state). */

/* ----- Suppress warnings about missing features ----- */
#define HAVE_ISNAN 0
#define HAVE_LOCALTIME_R 0
#define HAVE_LOCALTIME_S 0
#define HAVE_MALLOC_USABLE_SIZE 0
#define HAVE_STRCHRNUL 0
#define HAVE_USLEEP 0
#define HAVE_UTIME 0
#define HAVE_READLINK 0
#define HAVE_LSTAT 0
#define HAVE_FCHOWN 0

#endif /* HEAVEN_SQLITE_CONFIG_H */
