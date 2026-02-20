/*
 * heaven_lua_stubs.c — libc stubs for bare-metal Lua 5.4.8.
 *
 * Lua needs more libc than SQLite. We provide:
 * - stb_sprintf for snprintf/sprintf/vsnprintf (replaces the SQLite stubs
 *   for Lua compilation — SQLite has its own stubs in heaven_stubs.c)
 * - localeconv stub
 * - strcoll stub
 * - strerror stub
 * - getenv stub
 * - exit/abort (-> kernel halt)
 * - clock/time stubs for os-less environment
 *
 * Functions already provided by heaven_stubs.c (SQLite):
 *   strlen, strcmp, strncmp, strchr, strrchr, strcpy, strncpy, strcat,
 *   memchr, strtod, strtol, strtoul, strtoll, strtoull, atof, atoi,
 *   isdigit, isalpha, isalnum, isspace, toupper, tolower, isxdigit,
 *   pow, floor, ceil, fmod, sqrt, fabs, log, log2, log10, exp,
 *   frexp, ldexp, isnan, isinf
 *
 * Functions provided by compiler_builtins (Rust build-std):
 *   memcpy, memset, memcmp, memmove
 */

#include <stddef.h>
#include <stdarg.h>
#include <stdint.h>

/* ===== localeconv — Lua uses this for decimal point detection ===== */
struct lconv {
    char *decimal_point;
    char *thousands_sep;
    char *grouping;
    char *int_curr_symbol;
    char *currency_symbol;
    char *mon_decimal_point;
    char *mon_thousands_sep;
    char *mon_grouping;
    char *positive_sign;
    char *negative_sign;
    char int_frac_digits;
    char frac_digits;
    char p_cs_precedes;
    char p_sep_by_space;
    char n_cs_precedes;
    char n_sep_by_space;
    char p_sign_posn;
    char n_sign_posn;
};

static struct lconv _heaven_lconv = {
    .decimal_point = ".",
    .thousands_sep = "",
    .grouping = "",
    .int_curr_symbol = "",
    .currency_symbol = "",
    .mon_decimal_point = "",
    .mon_thousands_sep = "",
    .mon_grouping = "",
    .positive_sign = "",
    .negative_sign = "-",
};

struct lconv *localeconv(void) {
    return &_heaven_lconv;
}

/* ===== strcoll — no locale support, delegate to strcmp ===== */
int strcmp(const char *, const char *);  /* from heaven_stubs.c */

int strcoll(const char *s1, const char *s2) {
    return strcmp(s1, s2);
}

/* ===== strerror ===== */
char *strerror(int errnum) {
    (void)errnum;
    return "error";
}

/* ===== getenv — no environment in bare-metal ===== */
char *getenv(const char *name) {
    (void)name;
    return (void *)0;
}

/* ===== strstr — Lua uses this ===== */
char *strstr(const char *haystack, const char *needle) {
    if (!*needle) return (char *)haystack;
    for (; *haystack; haystack++) {
        const char *h = haystack, *n = needle;
        while (*h && *n && *h == *n) { h++; n++; }
        if (!*n) return (char *)haystack;
    }
    return (void *)0;
}

/* ===== strncat — Lua's lauxlib uses this ===== */
size_t strlen(const char *);  /* from heaven_stubs.c */

char *strncat(char *dst, const char *src, size_t n) {
    char *d = dst + strlen(dst);
    while (n > 0 && *src) { *d++ = *src++; n--; }
    *d = '\0';
    return dst;
}

/* ===== Serial output bridge (called by luaconf_heaven.h macros) ===== */

/* Implemented in Rust (kernel/src/lua/mod.rs) */
extern void serial_write_bytes(const char *s, int len);

void heaven_serial_write(const char *s, int len) {
    serial_write_bytes(s, len);
}

int heaven_strlen(const char *s) {
    return (int)strlen(s);
}

/* Forward to the existing snprintf from heaven_stubs.c */
int snprintf(char *buf, size_t n, const char *fmt, ...);
int vsnprintf(char *buf, size_t n, const char *fmt, va_list ap);

int heaven_snprintf(char *buf, int count, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, (size_t)count, fmt, ap);
    va_end(ap);
    return ret;
}

/* ===== exit — redirect to kernel panic ===== */
/* abort() is already defined in heaven_stubs.c (SQLite) */
extern void rust_panic_halt(void);

void exit(int status) {
    (void)status;
    rust_panic_halt();
}

/* ===== clock — Lua math.random seed uses this ===== */
typedef long clock_t;
clock_t clock(void) {
    return -1;  /* not available */
}

/* ===== time — Lua os.time / os.clock (not loaded, but referenced) ===== */
typedef long time_t;
time_t time(time_t *t);  /* already in heaven_stubs.c */

/* ===== abs — used by lcode.c (guard against INT_MIN UB) ===== */
int abs(int x) {
    if (x == (-2147483647 - 1)) return 2147483647;
    return x < 0 ? -x : x;
}

/* ===== strpbrk — used by lobject.c, lstrlib.c ===== */
char *strpbrk(const char *s, const char *accept) {
    for (; *s; s++) {
        for (const char *a = accept; *a; a++) {
            if (*s == *a) return (char *)s;
        }
    }
    return (void *)0;
}

/* ===== errno — glibc uses __errno_location for thread-local errno ===== */
static int _heaven_errno = 0;
int *__errno_location(void) { return &_heaven_errno; }

/* ===== FILE I/O stubs — lauxlib.c references these even if io lib not loaded ===== */
typedef struct _FILE { int unused; } FILE;
static FILE _stdin_placeholder;
FILE *stdin = &_stdin_placeholder;

FILE *fopen64(const char *path, const char *mode) { (void)path; (void)mode; return (void*)0; }
FILE *freopen64(const char *path, const char *mode, FILE *f) { (void)path; (void)mode; (void)f; return (void*)0; }
int fclose(FILE *f) { (void)f; return 0; }
size_t fread(void *buf, size_t size, size_t n, FILE *f) { (void)buf; (void)size; (void)n; (void)f; return 0; }
int feof(FILE *f) { (void)f; return 1; }
int ferror(FILE *f) { (void)f; return 1; }
int getc(FILE *f) { (void)f; return -1; /* EOF */ }
int ungetc(int c, FILE *f) { (void)c; (void)f; return -1; }

/* ===== glibc ctype stubs ===== */
/* glibc's ctype macros call __ctype_b_loc() to get a table of character flags.
 * We provide a static ASCII-only table. The table is indexed by (c + 128)
 * to handle EOF (-1) and negative chars. */

/* glibc ctype bit flags */
#define _ISupper    0x0100
#define _ISlower    0x0200
#define _ISalpha    0x0400
#define _ISdigit    0x0800
#define _ISxdigit   0x1000
#define _ISspace    0x2000
#define _ISprint    0x4000
#define _ISblank    0x0001
#define _IScntrl    0x0002
#define _ISpunct    0x0004
#define _ISalnum    0x0008

static const unsigned short _heaven_ctype_table[384] = {
    /* -128 .. -1: all zero (non-ASCII / EOF) */
    [0 ... 127] = 0,
    /* 0..31: control characters */
    [128 + 0 ... 128 + 8]   = _IScntrl,
    [128 + 9]  = _IScntrl | _ISspace | _ISblank,  /* \t */
    [128 + 10] = _IScntrl | _ISspace,              /* \n */
    [128 + 11] = _IScntrl | _ISspace,              /* \v */
    [128 + 12] = _IScntrl | _ISspace,              /* \f */
    [128 + 13] = _IScntrl | _ISspace,              /* \r */
    [128 + 14 ... 128 + 31] = _IScntrl,
    /* 32 = space */
    [128 + 32] = _ISprint | _ISspace | _ISblank,
    /* 33..47: punctuation */
    [128 + 33 ... 128 + 47] = _ISprint | _ISpunct,
    /* 48..57: digits */
    [128 + 48 ... 128 + 57] = _ISprint | _ISdigit | _ISxdigit | _ISalnum,
    /* 58..64: punctuation */
    [128 + 58 ... 128 + 64] = _ISprint | _ISpunct,
    /* 65..70: A-F (upper hex) */
    [128 + 65 ... 128 + 70] = _ISprint | _ISupper | _ISalpha | _ISxdigit | _ISalnum,
    /* 71..90: G-Z (upper) */
    [128 + 71 ... 128 + 90] = _ISprint | _ISupper | _ISalpha | _ISalnum,
    /* 91..96: punctuation */
    [128 + 91 ... 128 + 96] = _ISprint | _ISpunct,
    /* 97..102: a-f (lower hex) */
    [128 + 97 ... 128 + 102] = _ISprint | _ISlower | _ISalpha | _ISxdigit | _ISalnum,
    /* 103..122: g-z (lower) */
    [128 + 103 ... 128 + 122] = _ISprint | _ISlower | _ISalpha | _ISalnum,
    /* 123..126: punctuation */
    [128 + 123 ... 128 + 126] = _ISprint | _ISpunct,
    /* 127 = DEL */
    [128 + 127] = _IScntrl,
    /* 128..255: non-ASCII (zero) */
    [256 ... 383] = 0,
};

static const unsigned short *_heaven_ctype_ptr = &_heaven_ctype_table[128];

const unsigned short **__ctype_b_loc(void) {
    return &_heaven_ctype_ptr;
}

/* toupper/tolower tables: indexed by (c + 128) */
static const int _heaven_toupper_table[384] = {
    /* -128 .. -1 */
    [0 ... 127] = 0,
    /* 0..96: identity */
    [128 + 0] = 0, [128 + 1] = 1, [128 + 2] = 2, [128 + 3] = 3,
    [128 + 4] = 4, [128 + 5] = 5, [128 + 6] = 6, [128 + 7] = 7,
    [128 + 8] = 8, [128 + 9] = 9, [128 + 10] = 10, [128 + 11] = 11,
    [128 + 12] = 12, [128 + 13] = 13, [128 + 14] = 14, [128 + 15] = 15,
    [128 + 16] = 16, [128 + 17] = 17, [128 + 18] = 18, [128 + 19] = 19,
    [128 + 20] = 20, [128 + 21] = 21, [128 + 22] = 22, [128 + 23] = 23,
    [128 + 24] = 24, [128 + 25] = 25, [128 + 26] = 26, [128 + 27] = 27,
    [128 + 28] = 28, [128 + 29] = 29, [128 + 30] = 30, [128 + 31] = 31,
    [128 + 32] = 32, [128 + 33] = 33, [128 + 34] = 34, [128 + 35] = 35,
    [128 + 36] = 36, [128 + 37] = 37, [128 + 38] = 38, [128 + 39] = 39,
    [128 + 40] = 40, [128 + 41] = 41, [128 + 42] = 42, [128 + 43] = 43,
    [128 + 44] = 44, [128 + 45] = 45, [128 + 46] = 46, [128 + 47] = 47,
    [128 + 48] = 48, [128 + 49] = 49, [128 + 50] = 50, [128 + 51] = 51,
    [128 + 52] = 52, [128 + 53] = 53, [128 + 54] = 54, [128 + 55] = 55,
    [128 + 56] = 56, [128 + 57] = 57, [128 + 58] = 58, [128 + 59] = 59,
    [128 + 60] = 60, [128 + 61] = 61, [128 + 62] = 62, [128 + 63] = 63,
    [128 + 64] = 64, [128 + 65] = 65, [128 + 66] = 66, [128 + 67] = 67,
    [128 + 68] = 68, [128 + 69] = 69, [128 + 70] = 70, [128 + 71] = 71,
    [128 + 72] = 72, [128 + 73] = 73, [128 + 74] = 74, [128 + 75] = 75,
    [128 + 76] = 76, [128 + 77] = 77, [128 + 78] = 78, [128 + 79] = 79,
    [128 + 80] = 80, [128 + 81] = 81, [128 + 82] = 82, [128 + 83] = 83,
    [128 + 84] = 84, [128 + 85] = 85, [128 + 86] = 86, [128 + 87] = 87,
    [128 + 88] = 88, [128 + 89] = 89, [128 + 90] = 90, [128 + 91] = 91,
    [128 + 92] = 92, [128 + 93] = 93, [128 + 94] = 94, [128 + 95] = 95,
    [128 + 96] = 96,
    /* 97..122: a-z -> A-Z */
    [128 + 97] = 65, [128 + 98] = 66, [128 + 99] = 67, [128 + 100] = 68,
    [128 + 101] = 69, [128 + 102] = 70, [128 + 103] = 71, [128 + 104] = 72,
    [128 + 105] = 73, [128 + 106] = 74, [128 + 107] = 75, [128 + 108] = 76,
    [128 + 109] = 77, [128 + 110] = 78, [128 + 111] = 79, [128 + 112] = 80,
    [128 + 113] = 81, [128 + 114] = 82, [128 + 115] = 83, [128 + 116] = 84,
    [128 + 117] = 85, [128 + 118] = 86, [128 + 119] = 87, [128 + 120] = 88,
    [128 + 121] = 89, [128 + 122] = 90,
    /* 123..127: identity */
    [128 + 123] = 123, [128 + 124] = 124, [128 + 125] = 125, [128 + 126] = 126,
    [128 + 127] = 127,
    /* 128..255: identity (non-ASCII passthrough) */
    [256] = 128, [257] = 129, [258] = 130, [259] = 131,
    [260] = 132, [261] = 133, [262] = 134, [263] = 135,
    [264] = 136, [265] = 137, [266] = 138, [267] = 139,
    [268] = 140, [269] = 141, [270] = 142, [271] = 143,
    [272] = 144, [273] = 145, [274] = 146, [275] = 147,
    [276] = 148, [277] = 149, [278] = 150, [279] = 151,
    [280] = 152, [281] = 153, [282] = 154, [283] = 155,
    [284] = 156, [285] = 157, [286] = 158, [287] = 159,
    [288] = 160, [289] = 161, [290] = 162, [291] = 163,
    [292] = 164, [293] = 165, [294] = 166, [295] = 167,
    [296] = 168, [297] = 169, [298] = 170, [299] = 171,
    [300] = 172, [301] = 173, [302] = 174, [303] = 175,
    [304] = 176, [305] = 177, [306] = 178, [307] = 179,
    [308] = 180, [309] = 181, [310] = 182, [311] = 183,
    [312] = 184, [313] = 185, [314] = 186, [315] = 187,
    [316] = 188, [317] = 189, [318] = 190, [319] = 191,
    [320] = 192, [321] = 193, [322] = 194, [323] = 195,
    [324] = 196, [325] = 197, [326] = 198, [327] = 199,
    [328] = 200, [329] = 201, [330] = 202, [331] = 203,
    [332] = 204, [333] = 205, [334] = 206, [335] = 207,
    [336] = 208, [337] = 209, [338] = 210, [339] = 211,
    [340] = 212, [341] = 213, [342] = 214, [343] = 215,
    [344] = 216, [345] = 217, [346] = 218, [347] = 219,
    [348] = 220, [349] = 221, [350] = 222, [351] = 223,
    [352] = 224, [353] = 225, [354] = 226, [355] = 227,
    [356] = 228, [357] = 229, [358] = 230, [359] = 231,
    [360] = 232, [361] = 233, [362] = 234, [363] = 235,
    [364] = 236, [365] = 237, [366] = 238, [367] = 239,
    [368] = 240, [369] = 241, [370] = 242, [371] = 243,
    [372] = 244, [373] = 245, [374] = 246, [375] = 247,
    [376] = 248, [377] = 249, [378] = 250, [379] = 251,
    [380] = 252, [381] = 253, [382] = 254, [383] = 255,
};

static const int *_heaven_toupper_ptr = &_heaven_toupper_table[128];

const int **__ctype_toupper_loc(void) {
    return &_heaven_toupper_ptr;
}

static const int _heaven_tolower_table[384] = {
    /* -128 .. -1 */
    [0 ... 127] = 0,
    /* 0..64: identity */
    [128 + 0] = 0, [128 + 1] = 1, [128 + 2] = 2, [128 + 3] = 3,
    [128 + 4] = 4, [128 + 5] = 5, [128 + 6] = 6, [128 + 7] = 7,
    [128 + 8] = 8, [128 + 9] = 9, [128 + 10] = 10, [128 + 11] = 11,
    [128 + 12] = 12, [128 + 13] = 13, [128 + 14] = 14, [128 + 15] = 15,
    [128 + 16] = 16, [128 + 17] = 17, [128 + 18] = 18, [128 + 19] = 19,
    [128 + 20] = 20, [128 + 21] = 21, [128 + 22] = 22, [128 + 23] = 23,
    [128 + 24] = 24, [128 + 25] = 25, [128 + 26] = 26, [128 + 27] = 27,
    [128 + 28] = 28, [128 + 29] = 29, [128 + 30] = 30, [128 + 31] = 31,
    [128 + 32] = 32, [128 + 33] = 33, [128 + 34] = 34, [128 + 35] = 35,
    [128 + 36] = 36, [128 + 37] = 37, [128 + 38] = 38, [128 + 39] = 39,
    [128 + 40] = 40, [128 + 41] = 41, [128 + 42] = 42, [128 + 43] = 43,
    [128 + 44] = 44, [128 + 45] = 45, [128 + 46] = 46, [128 + 47] = 47,
    [128 + 48] = 48, [128 + 49] = 49, [128 + 50] = 50, [128 + 51] = 51,
    [128 + 52] = 52, [128 + 53] = 53, [128 + 54] = 54, [128 + 55] = 55,
    [128 + 56] = 56, [128 + 57] = 57, [128 + 58] = 58, [128 + 59] = 59,
    [128 + 60] = 60, [128 + 61] = 61, [128 + 62] = 62, [128 + 63] = 63,
    [128 + 64] = 64,
    /* 65..90: A-Z -> a-z */
    [128 + 65] = 97, [128 + 66] = 98, [128 + 67] = 99, [128 + 68] = 100,
    [128 + 69] = 101, [128 + 70] = 102, [128 + 71] = 103, [128 + 72] = 104,
    [128 + 73] = 105, [128 + 74] = 106, [128 + 75] = 107, [128 + 76] = 108,
    [128 + 77] = 109, [128 + 78] = 110, [128 + 79] = 111, [128 + 80] = 112,
    [128 + 81] = 113, [128 + 82] = 114, [128 + 83] = 115, [128 + 84] = 116,
    [128 + 85] = 117, [128 + 86] = 118, [128 + 87] = 119, [128 + 88] = 120,
    [128 + 89] = 121, [128 + 90] = 122,
    /* 91..127: identity */
    [128 + 91] = 91, [128 + 92] = 92, [128 + 93] = 93, [128 + 94] = 94,
    [128 + 95] = 95, [128 + 96] = 96,
    [128 + 97] = 97, [128 + 98] = 98, [128 + 99] = 99, [128 + 100] = 100,
    [128 + 101] = 101, [128 + 102] = 102, [128 + 103] = 103, [128 + 104] = 104,
    [128 + 105] = 105, [128 + 106] = 106, [128 + 107] = 107, [128 + 108] = 108,
    [128 + 109] = 109, [128 + 110] = 110, [128 + 111] = 111, [128 + 112] = 112,
    [128 + 113] = 113, [128 + 114] = 114, [128 + 115] = 115, [128 + 116] = 116,
    [128 + 117] = 117, [128 + 118] = 118, [128 + 119] = 119, [128 + 120] = 120,
    [128 + 121] = 121, [128 + 122] = 122, [128 + 123] = 123, [128 + 124] = 124,
    [128 + 125] = 125, [128 + 126] = 126, [128 + 127] = 127,
    /* 128..255: identity (non-ASCII passthrough) */
    [256] = 128, [257] = 129, [258] = 130, [259] = 131,
    [260] = 132, [261] = 133, [262] = 134, [263] = 135,
    [264] = 136, [265] = 137, [266] = 138, [267] = 139,
    [268] = 140, [269] = 141, [270] = 142, [271] = 143,
    [272] = 144, [273] = 145, [274] = 146, [275] = 147,
    [276] = 148, [277] = 149, [278] = 150, [279] = 151,
    [280] = 152, [281] = 153, [282] = 154, [283] = 155,
    [284] = 156, [285] = 157, [286] = 158, [287] = 159,
    [288] = 160, [289] = 161, [290] = 162, [291] = 163,
    [292] = 164, [293] = 165, [294] = 166, [295] = 167,
    [296] = 168, [297] = 169, [298] = 170, [299] = 171,
    [300] = 172, [301] = 173, [302] = 174, [303] = 175,
    [304] = 176, [305] = 177, [306] = 178, [307] = 179,
    [308] = 180, [309] = 181, [310] = 182, [311] = 183,
    [312] = 184, [313] = 185, [314] = 186, [315] = 187,
    [316] = 188, [317] = 189, [318] = 190, [319] = 191,
    [320] = 192, [321] = 193, [322] = 194, [323] = 195,
    [324] = 196, [325] = 197, [326] = 198, [327] = 199,
    [328] = 200, [329] = 201, [330] = 202, [331] = 203,
    [332] = 204, [333] = 205, [334] = 206, [335] = 207,
    [336] = 208, [337] = 209, [338] = 210, [339] = 211,
    [340] = 212, [341] = 213, [342] = 214, [343] = 215,
    [344] = 216, [345] = 217, [346] = 218, [347] = 219,
    [348] = 220, [349] = 221, [350] = 222, [351] = 223,
    [352] = 224, [353] = 225, [354] = 226, [355] = 227,
    [356] = 228, [357] = 229, [358] = 230, [359] = 231,
    [360] = 232, [361] = 233, [362] = 234, [363] = 235,
    [364] = 236, [365] = 237, [366] = 238, [367] = 239,
    [368] = 240, [369] = 241, [370] = 242, [371] = 243,
    [372] = 244, [373] = 245, [374] = 246, [375] = 247,
    [376] = 248, [377] = 249, [378] = 250, [379] = 251,
    [380] = 252, [381] = 253, [382] = 254, [383] = 255,
};

static const int *_heaven_tolower_ptr = &_heaven_tolower_table[128];

const int **__ctype_tolower_loc(void) {
    return &_heaven_tolower_ptr;
}

/* ===== Additional math: atan2, sin, cos, tan, asin, acos, atan ===== */
/* These are used by lmathlib.c. We provide minimal implementations
 * using Taylor series. For a production system, use openlibm. */

double sin(double x);
double cos(double x);
double atan2(double y, double x);
double asin(double x);
double acos(double x);
double tan(double x);

/* Reduce x to [-pi, pi] range */
static double _reduce_angle(double x) {
    const double TWO_PI = 6.28318530717958648;
    const double PI = 3.14159265358979324;
    x = x - TWO_PI * (double)((long long)(x / TWO_PI));
    if (x > PI) x -= TWO_PI;
    if (x < -PI) x += TWO_PI;
    return x;
}

double sin(double x) {
    x = _reduce_angle(x);
    /* Taylor: x - x^3/3! + x^5/5! - x^7/7! + ... */
    double term = x;
    double sum = x;
    for (int i = 1; i <= 12; i++) {
        term *= -x * x / (double)((2*i) * (2*i + 1));
        sum += term;
    }
    return sum;
}

double cos(double x) {
    x = _reduce_angle(x);
    double term = 1.0;
    double sum = 1.0;
    for (int i = 1; i <= 12; i++) {
        term *= -x * x / (double)((2*i - 1) * (2*i));
        sum += term;
    }
    return sum;
}

double tan(double x) {
    double c = cos(x);
    if (c == 0.0) return 1.0e308;  /* infinity approximation */
    return sin(x) / c;
}

double fabs(double);   /* from heaven_stubs.c */
double sqrt(double);   /* from heaven_stubs.c */

double atan(double x) {
    /* For |x| > 1, use identity: atan(x) = pi/2 - atan(1/x) */
    const double PI_2 = 1.57079632679489662;
    if (x > 1.0) return PI_2 - atan(1.0 / x);
    if (x < -1.0) return -PI_2 - atan(1.0 / x);
    /* Taylor: x - x^3/3 + x^5/5 - x^7/7 + ... (|x| <= 1) */
    double term = x;
    double sum = x;
    double x2 = x * x;
    for (int i = 1; i <= 20; i++) {
        term *= -x2;
        sum += term / (double)(2*i + 1);
    }
    return sum;
}

double atan2(double y, double x) {
    const double PI = 3.14159265358979324;
    if (x > 0) return atan(y / x);
    if (x < 0 && y >= 0) return atan(y / x) + PI;
    if (x < 0 && y < 0) return atan(y / x) - PI;
    if (x == 0 && y > 0) return PI / 2.0;
    if (x == 0 && y < 0) return -PI / 2.0;
    return 0.0;  /* x == 0, y == 0 */
}

double asin(double x) {
    /* asin(x) = atan2(x, sqrt(1 - x*x)) */
    if (x >= 1.0) return 1.57079632679489662;
    if (x <= -1.0) return -1.57079632679489662;
    return atan2(x, sqrt(1.0 - x * x));
}

double acos(double x) {
    return 1.57079632679489662 - asin(x);
}

/* fmin/fmax — Lua 5.4 math library uses these (NaN-correct per C99) */
int isnan(double);  /* from heaven_stubs.c */
double fmin(double a, double b) {
    if (isnan(a)) return b;
    if (isnan(b)) return a;
    return a < b ? a : b;
}
double fmax(double a, double b) {
    if (isnan(a)) return b;
    if (isnan(b)) return a;
    return a > b ? a : b;
}
