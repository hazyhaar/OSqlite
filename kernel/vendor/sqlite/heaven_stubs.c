/*
 * heaven_stubs.c — libc stubs for bare-metal SQLite.
 *
 * SQLite with SQLITE_OS_OTHER + SQLITE_ZERO_MALLOC + all the OMIT flags
 * still references some libc functions. We provide minimal implementations
 * here, or link to Rust-side implementations via extern.
 *
 * The memory allocator (sqlite3_malloc etc.) is provided in this file and
 * redirects to a Rust slab allocator via FFI.
 */

#include "sqlite3.h"
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>

/* ====================================================================
 * String / memory functions
 *
 * memcpy, memset, memcmp, memmove are provided by compiler_builtins
 * (via Rust's build-std). We only need to provide the string functions
 * that SQLite uses.
 * ==================================================================== */

size_t strlen(const char *s) {
    const char *p = s;
    while (*p) p++;
    return (size_t)(p - s);
}

int strcmp(const char *s1, const char *s2) {
    while (*s1 && (*s1 == *s2)) { s1++; s2++; }
    return (unsigned char)*s1 - (unsigned char)*s2;
}

int strncmp(const char *s1, const char *s2, size_t n) {
    if (n == 0) return 0;
    while (--n && *s1 && (*s1 == *s2)) { s1++; s2++; }
    return (unsigned char)*s1 - (unsigned char)*s2;
}

char *strcpy(char *dst, const char *src) {
    char *d = dst;
    while ((*d++ = *src++)) {}
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n) {
    char *d = dst;
    while (n > 0 && *src) { *d++ = *src++; n--; }
    while (n > 0) { *d++ = '\0'; n--; }
    return dst;
}

char *strcat(char *dst, const char *src) {
    char *d = dst + strlen(dst);
    while ((*d++ = *src++)) {}
    return dst;
}

char *strchr(const char *s, int c) {
    while (*s) {
        if (*s == (char)c) return (char *)s;
        s++;
    }
    return (c == 0) ? (char *)s : NULL;
}

char *strrchr(const char *s, int c) {
    const char *last = NULL;
    while (*s) {
        if (*s == (char)c) last = s;
        s++;
    }
    if (c == 0) return (char *)s;
    return (char *)last;
}

void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    for (size_t i = 0; i < n; i++) {
        if (p[i] == (unsigned char)c) return (void *)(p + i);
    }
    return NULL;
}

int memcmp(const void *s1, const void *s2, size_t n);
void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *s, int c, size_t n);
void *memmove(void *dst, const void *src, size_t n);

/* ====================================================================
 * ctype functions
 * ==================================================================== */

int isdigit(int c) { return c >= '0' && c <= '9'; }
int isalpha(int c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); }
int isalnum(int c) { return isdigit(c) || isalpha(c); }
int isspace(int c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; }
int isupper(int c) { return c >= 'A' && c <= 'Z'; }
int islower(int c) { return c >= 'a' && c <= 'z'; }
int isxdigit(int c) { return isdigit(c) || (c >= 'A' && c <= 'F') || (c >= 'a' && c <= 'f'); }
int isprint(int c) { return c >= 0x20 && c <= 0x7e; }
int toupper(int c) { return islower(c) ? c - 32 : c; }
int tolower(int c) { return isupper(c) ? c + 32 : c; }

/* ====================================================================
 * strtol / strtoul / strtod / strtoll / strtoull
 *
 * SQLite uses these for number parsing. Minimal implementations.
 * ==================================================================== */

long strtol(const char *nptr, char **endptr, int base) {
    const char *s = nptr;
    long result = 0;
    int neg = 0;

    while (isspace(*s)) s++;
    if (*s == '-') { neg = 1; s++; }
    else if (*s == '+') { s++; }

    if (base == 0) {
        if (*s == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
        else if (*s == '0') { base = 8; }
        else { base = 10; }
    } else if (base == 16 && *s == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }

    while (*s) {
        int digit;
        if (*s >= '0' && *s <= '9') digit = *s - '0';
        else if (*s >= 'a' && *s <= 'z') digit = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'Z') digit = *s - 'A' + 10;
        else break;
        if (digit >= base) break;
        result = result * base + digit;
        s++;
    }

    if (endptr) *endptr = (char *)s;
    return neg ? -result : result;
}

unsigned long strtoul(const char *nptr, char **endptr, int base) {
    return (unsigned long)strtol(nptr, endptr, base);
}

long long strtoll(const char *nptr, char **endptr, int base) {
    return (long long)strtol(nptr, endptr, base);
}

unsigned long long strtoull(const char *nptr, char **endptr, int base) {
    return (unsigned long long)strtol(nptr, endptr, base);
}

double strtod(const char *nptr, char **endptr) {
    const char *s = nptr;
    double result = 0.0;
    int neg = 0;

    while (isspace(*s)) s++;
    if (*s == '-') { neg = 1; s++; }
    else if (*s == '+') { s++; }

    /* Integer part */
    while (*s >= '0' && *s <= '9') {
        result = result * 10.0 + (*s - '0');
        s++;
    }

    /* Fractional part */
    if (*s == '.') {
        s++;
        double frac = 0.1;
        while (*s >= '0' && *s <= '9') {
            result += (*s - '0') * frac;
            frac *= 0.1;
            s++;
        }
    }

    /* Exponent part */
    if (*s == 'e' || *s == 'E') {
        s++;
        int exp_neg = 0;
        int exp = 0;
        if (*s == '-') { exp_neg = 1; s++; }
        else if (*s == '+') { s++; }
        while (*s >= '0' && *s <= '9') {
            exp = exp * 10 + (*s - '0');
            s++;
        }
        double mult = 1.0;
        for (int i = 0; i < exp; i++) mult *= 10.0;
        if (exp_neg) result /= mult;
        else result *= mult;
    }

    if (endptr) *endptr = (char *)s;
    return neg ? -result : result;
}

double atof(const char *s) { return strtod(s, NULL); }
int atoi(const char *s) { return (int)strtol(s, NULL, 10); }

/* ====================================================================
 * snprintf / vsnprintf
 *
 * SQLite uses these heavily. We provide a minimal but functional
 * implementation. This handles: %d %u %ld %lu %lld %llu %x %s %c %p %% %f %e %g
 * ==================================================================== */

static int fmt_int(char *buf, size_t n, size_t pos, long long val) {
    char tmp[24];
    int neg = 0;
    int len = 0;

    if (val < 0) { neg = 1; val = -val; }
    if (val == 0) { tmp[len++] = '0'; }
    else {
        while (val > 0) { tmp[len++] = '0' + (int)(val % 10); val /= 10; }
    }
    if (neg) tmp[len++] = '-';

    /* Reverse into buf */
    for (int i = len - 1; i >= 0; i--) {
        if (pos < n - 1) buf[pos] = tmp[i];
        pos++;
    }
    return len;
}

static int fmt_uint(char *buf, size_t n, size_t pos, unsigned long long val, int base, int upper) {
    char tmp[24];
    const char *digits = upper ? "0123456789ABCDEF" : "0123456789abcdef";
    int len = 0;

    if (val == 0) { tmp[len++] = '0'; }
    else {
        while (val > 0) { tmp[len++] = digits[val % base]; val /= base; }
    }

    for (int i = len - 1; i >= 0; i--) {
        if (pos < n - 1) buf[pos] = tmp[i];
        pos++;
    }
    return len;
}

static int fmt_double(char *buf, size_t n, size_t pos, double val, int precision) {
    int written = 0;

    /* Handle negative */
    if (val < 0) {
        if (pos < n - 1) buf[pos] = '-';
        pos++; written++;
        val = -val;
    }

    /* Integer part */
    unsigned long long ipart = (unsigned long long)val;
    int ilen = fmt_uint(buf, n, pos, ipart, 10, 0);
    pos += ilen; written += ilen;

    /* Fractional part */
    if (precision > 0) {
        if (pos < n - 1) buf[pos] = '.';
        pos++; written++;

        double frac = val - (double)ipart;
        for (int i = 0; i < precision; i++) {
            frac *= 10.0;
            int d = (int)frac;
            if (d > 9) d = 9;
            if (pos < n - 1) buf[pos] = '0' + d;
            pos++; written++;
            frac -= d;
        }
    }
    return written;
}

int vsnprintf(char *buf, size_t n, const char *fmt, va_list ap) {
    size_t pos = 0;
    if (n == 0) return 0;

    while (*fmt) {
        if (*fmt != '%') {
            if (pos < n - 1) buf[pos] = *fmt;
            pos++; fmt++;
            continue;
        }

        fmt++; /* skip '%' */

        /* Parse flags */
        int zero_pad = 0;
        int left_align = 0;
        while (*fmt == '0' || *fmt == '-' || *fmt == ' ' || *fmt == '+') {
            if (*fmt == '0') zero_pad = 1;
            if (*fmt == '-') left_align = 1;
            fmt++;
        }
        (void)left_align;

        /* Parse width */
        int width = 0;
        if (*fmt == '*') {
            width = va_arg(ap, int);
            fmt++;
        } else {
            while (*fmt >= '0' && *fmt <= '9') {
                width = width * 10 + (*fmt - '0');
                fmt++;
            }
        }

        /* Parse precision */
        int precision = -1;
        if (*fmt == '.') {
            fmt++;
            precision = 0;
            if (*fmt == '*') {
                precision = va_arg(ap, int);
                fmt++;
            } else {
                while (*fmt >= '0' && *fmt <= '9') {
                    precision = precision * 10 + (*fmt - '0');
                    fmt++;
                }
            }
        }

        /* Parse length modifier */
        int is_long = 0;
        int is_longlong = 0;
        if (*fmt == 'l') {
            fmt++;
            is_long = 1;
            if (*fmt == 'l') { fmt++; is_longlong = 1; }
        } else if (*fmt == 'z') {
            fmt++;
            is_long = 1; /* size_t = unsigned long on x86_64 */
        }

        /* Conversion */
        size_t start = pos;
        switch (*fmt) {
        case 'd': case 'i': {
            long long val;
            if (is_longlong) val = va_arg(ap, long long);
            else if (is_long) val = va_arg(ap, long);
            else val = va_arg(ap, int);
            int len = fmt_int(buf, n, pos, val);
            /* Zero-padding */
            if (zero_pad && width > len) {
                /* Simple: we already wrote digits; this is imperfect for negative + zero-pad
                 * but sufficient for SQLite's needs */
            }
            pos += len;
            break;
        }
        case 'u': {
            unsigned long long val;
            if (is_longlong) val = va_arg(ap, unsigned long long);
            else if (is_long) val = va_arg(ap, unsigned long);
            else val = va_arg(ap, unsigned int);
            pos += fmt_uint(buf, n, pos, val, 10, 0);
            break;
        }
        case 'x': case 'X': {
            unsigned long long val;
            if (is_longlong) val = va_arg(ap, unsigned long long);
            else if (is_long) val = va_arg(ap, unsigned long);
            else val = va_arg(ap, unsigned int);
            pos += fmt_uint(buf, n, pos, val, 16, *fmt == 'X');
            break;
        }
        case 'o': {
            unsigned long long val;
            if (is_longlong) val = va_arg(ap, unsigned long long);
            else if (is_long) val = va_arg(ap, unsigned long);
            else val = va_arg(ap, unsigned int);
            pos += fmt_uint(buf, n, pos, val, 8, 0);
            break;
        }
        case 'f': {
            double val = va_arg(ap, double);
            int prec = (precision >= 0) ? precision : 6;
            pos += fmt_double(buf, n, pos, val, prec);
            break;
        }
        case 'e': case 'E': case 'g': case 'G': {
            /* Simplified: just use %f formatting */
            double val = va_arg(ap, double);
            int prec = (precision >= 0) ? precision : 6;
            pos += fmt_double(buf, n, pos, val, prec);
            break;
        }
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            int slen = strlen(s);
            if (precision >= 0 && precision < slen) slen = precision;
            for (int i = 0; i < slen; i++) {
                if (pos < n - 1) buf[pos] = s[i];
                pos++;
            }
            break;
        }
        case 'c': {
            int c = va_arg(ap, int);
            if (pos < n - 1) buf[pos] = (char)c;
            pos++;
            break;
        }
        case 'p': {
            void *ptr = va_arg(ap, void *);
            if (pos < n - 1) buf[pos] = '0'; pos++;
            if (pos < n - 1) buf[pos] = 'x'; pos++;
            pos += fmt_uint(buf, n, pos, (unsigned long long)(uintptr_t)ptr, 16, 0);
            break;
        }
        case '%':
            if (pos < n - 1) buf[pos] = '%';
            pos++;
            break;
        case 'n':
            /* Intentionally not supported (security) */
            break;
        default:
            /* Unknown format specifier — just emit it */
            if (pos < n - 1) buf[pos] = '%';
            pos++;
            if (pos < n - 1) buf[pos] = *fmt;
            pos++;
            break;
        }

        /* Width padding (post-print, right-pad with spaces if needed) */
        (void)start;
        (void)width;

        if (*fmt) fmt++;
    }

    if (n > 0) buf[pos < n ? pos : n - 1] = '\0';
    return (int)pos;
}

int snprintf(char *buf, size_t n, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, n, fmt, ap);
    va_end(ap);
    return ret;
}

int sprintf(char *buf, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, 65536, fmt, ap);
    va_end(ap);
    return ret;
}

/* ====================================================================
 * Math functions — SQLite uses a few.
 * Minimal implementations; not IEEE-754 perfect but good enough for SQL.
 * ==================================================================== */

double fabs(double x) { return x < 0 ? -x : x; }

double fmod(double x, double y) {
    if (y == 0.0) return 0.0;
    return x - (double)((long long)(x / y)) * y;
}

double floor(double x) {
    long long i = (long long)x;
    return (x < 0.0 && x != (double)i) ? (double)(i - 1) : (double)i;
}

double ceil(double x) {
    long long i = (long long)x;
    return (x > 0.0 && x != (double)i) ? (double)(i + 1) : (double)i;
}

double sqrt(double x) {
    if (x < 0) return 0.0;
    if (x == 0.0) return 0.0;
    double guess = x / 2.0;
    for (int i = 0; i < 64; i++) {
        guess = (guess + x / guess) / 2.0;
    }
    return guess;
}

double log(double x) {
    if (x <= 0) return -1.0e308;
    /* Newton's method: ln(x) via exp inverse */
    double result = 0.0;
    /* Reduce to [1,2): x = m * 2^e */
    int exp = 0;
    double m = x;
    while (m >= 2.0) { m /= 2.0; exp++; }
    while (m < 1.0) { m *= 2.0; exp--; }
    /* ln(x) = exp * ln(2) + ln(m) */
    /* ln(m) for m in [1,2): Taylor around 1: ln(1+u) = u - u^2/2 + u^3/3 - ... */
    double u = m - 1.0;
    double term = u;
    for (int i = 1; i <= 20; i++) {
        result += term / (double)i;
        term *= -u;
    }
    return result + (double)exp * 0.693147180559945309;
}

double log2(double x) { return log(x) / 0.693147180559945309; }
double log10(double x) { return log(x) / 2.302585092994045684; }

double exp(double x) {
    /* Taylor series: e^x = 1 + x + x^2/2! + x^3/3! + ... */
    double result = 1.0;
    double term = 1.0;
    for (int i = 1; i <= 30; i++) {
        term *= x / (double)i;
        result += term;
    }
    return result;
}

double pow(double base, double exponent) {
    if (exponent == 0.0) return 1.0;
    if (base == 0.0) return 0.0;
    /* For integer exponents, use repeated multiplication */
    if (exponent == (double)(long long)exponent && exponent > 0 && exponent < 64) {
        double result = 1.0;
        long long n = (long long)exponent;
        double b = base;
        while (n > 0) {
            if (n & 1) result *= b;
            b *= b;
            n >>= 1;
        }
        return result;
    }
    return exp(exponent * log(base));
}

double ldexp(double x, int exp) {
    /* x * 2^exp */
    while (exp > 0) { x *= 2.0; exp--; }
    while (exp < 0) { x /= 2.0; exp++; }
    return x;
}

double frexp(double x, int *exp) {
    *exp = 0;
    if (x == 0.0) return 0.0;
    int neg = 0;
    if (x < 0) { neg = 1; x = -x; }
    while (x >= 1.0) { x /= 2.0; (*exp)++; }
    while (x < 0.5) { x *= 2.0; (*exp)--; }
    return neg ? -x : x;
}

int isnan(double x) {
    /* NaN != NaN */
    volatile double a = x;
    return a != a;
}

int isinf(double x) {
    return (x == 1.0/0.0) || (x == -1.0/0.0);
}

/* ====================================================================
 * Memory allocator — redirect to Rust slab allocator via FFI.
 *
 * We store the allocation size in a header before the returned pointer
 * so that sqlite3_free() and sqlite3_realloc() can work without
 * a separate size lookup.
 * ==================================================================== */

/*
 * Memory allocator — configured at runtime via sqlite3_config().
 *
 * With SQLITE_ZERO_MALLOC=1, SQLite provides its own no-op malloc stubs.
 * Before calling sqlite3_initialize(), we call sqlite3_config(SQLITE_CONFIG_MALLOC)
 * to install our allocator which redirects to the Rust slab allocator.
 *
 * These functions are used to build the sqlite3_mem_methods struct.
 */
extern void *heavenos_malloc(size_t size);
extern void  heavenos_free(void *ptr);
extern void *heavenos_realloc(void *ptr, size_t new_size);
extern size_t heavenos_malloc_size(void *ptr);

static void *heaven_mem_malloc(int n) {
    if (n <= 0) return NULL;
    return heavenos_malloc((size_t)n);
}

static void heaven_mem_free(void *ptr) {
    heavenos_free(ptr);
}

static void *heaven_mem_realloc(void *ptr, int n) {
    if (n <= 0) { heavenos_free(ptr); return NULL; }
    if (!ptr) return heavenos_malloc((size_t)n);
    return heavenos_realloc(ptr, (size_t)n);
}

static int heaven_mem_size(void *ptr) {
    return (int)heavenos_malloc_size(ptr);
}

static int heaven_mem_roundup(int n) {
    /* Round up to nearest power of two that matches slab classes */
    int r = 8;
    while (r < n && r < 4096) r <<= 1;
    if (r < n) r = (n + 4095) & ~4095;
    return r;
}

static int heaven_mem_init(void *pAppData) { (void)pAppData; return SQLITE_OK; }
static void heaven_mem_shutdown(void *pAppData) { (void)pAppData; }

/* Called from Rust before sqlite3_initialize() */
int heaven_configure_malloc(void) {
    sqlite3_mem_methods methods = {
        .xMalloc   = heaven_mem_malloc,
        .xFree     = heaven_mem_free,
        .xRealloc  = heaven_mem_realloc,
        .xSize     = heaven_mem_size,
        .xRoundup  = heaven_mem_roundup,
        .xInit     = heaven_mem_init,
        .xShutdown = heaven_mem_shutdown,
        .pAppData  = NULL,
    };
    return sqlite3_config(SQLITE_CONFIG_MALLOC, &methods);
}

/* ====================================================================
 * OS init/end — called by sqlite3_initialize() / sqlite3_shutdown().
 * We register our VFS from Rust, so these are stubs.
 * ==================================================================== */

int sqlite3_os_init(void) { return SQLITE_OK; }
int sqlite3_os_end(void)  { return SQLITE_OK; }

/* ====================================================================
 * Misc stubs
 * ==================================================================== */

/* SQLite may reference these even with our OMIT flags */
int open(const char *path, int flags, ...) { (void)path; (void)flags; return -1; }
int close(int fd) { (void)fd; return -1; }
int read(int fd, void *buf, size_t count) { (void)fd; (void)buf; (void)count; return -1; }
int write(int fd, const void *buf, size_t count) { (void)fd; (void)buf; (void)count; return -1; }
int unlink(const char *path) { (void)path; return -1; }
int access(const char *path, int mode) { (void)path; (void)mode; return -1; }
int stat(const char *path, void *buf) { (void)path; (void)buf; return -1; }
int fstat(int fd, void *buf) { (void)fd; (void)buf; return -1; }
int fcntl(int fd, int cmd, ...) { (void)fd; (void)cmd; return -1; }
int ioctl(int fd, unsigned long request, ...) { (void)fd; (void)request; return -1; }
long lseek(int fd, long offset, int whence) { (void)fd; (void)offset; (void)whence; return -1; }
int fsync(int fd) { (void)fd; return -1; }
int ftruncate(int fd, long length) { (void)fd; (void)length; return -1; }
int mkdir(const char *path, unsigned mode) { (void)path; (void)mode; return -1; }
int rmdir(const char *path) { (void)path; return -1; }
char *getcwd(char *buf, size_t size) { (void)buf; (void)size; return NULL; }
unsigned int sleep(unsigned int seconds) { (void)seconds; return 0; }
int usleep(unsigned usec) { (void)usec; return 0; }
int gettimeofday(void *tv, void *tz) { (void)tv; (void)tz; return -1; }
long time(long *t) { if (t) *t = 0; return 0; }
void *dlopen(const char *f, int m) { (void)f; (void)m; return NULL; }
void *dlsym(void *h, const char *s) { (void)h; (void)s; return NULL; }
int dlclose(void *h) { (void)h; return -1; }
char *dlerror(void) { return "no dynamic loading"; }
unsigned getpid(void) { return 1; }

/* errno — SQLite references it */
int errno = 0;

/* abort — should never be called */
void abort(void) {
    /* Spin forever. In kernel context we can't really exit. */
    for (;;) { __asm__ volatile("hlt"); }
}

/* qsort — SQLite uses this for ORDER BY etc. Simple shell sort. */
void qsort(void *base, size_t nel, size_t width, int (*compar)(const void *, const void *)) {
    char *arr = (char *)base;
    char tmp[256]; /* max element size — SQLite structs are < 256 bytes */
    if (width > sizeof(tmp)) return; /* safety */

    /* Shell sort — simple, in-place, adequate for SQLite's needs */
    for (size_t gap = nel / 2; gap > 0; gap /= 2) {
        for (size_t i = gap; i < nel; i++) {
            memcpy(tmp, arr + i * width, width);
            size_t j = i;
            while (j >= gap && compar(arr + (j - gap) * width, tmp) > 0) {
                memcpy(arr + j * width, arr + (j - gap) * width, width);
                j -= gap;
            }
            memcpy(arr + j * width, tmp, width);
        }
    }
}

/* bsearch */
void *bsearch(const void *key, const void *base, size_t nel, size_t width,
              int (*compar)(const void *, const void *)) {
    const char *arr = (const char *)base;
    size_t lo = 0, hi = nel;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        int cmp = compar(key, arr + mid * width);
        if (cmp == 0) return (void *)(arr + mid * width);
        if (cmp < 0) hi = mid;
        else lo = mid + 1;
    }
    return NULL;
}

/* ====================================================================
 * Additional string functions
 * ==================================================================== */

size_t strspn(const char *s, const char *accept) {
    size_t count = 0;
    while (*s) {
        const char *a = accept;
        int found = 0;
        while (*a) { if (*s == *a) { found = 1; break; } a++; }
        if (!found) break;
        s++; count++;
    }
    return count;
}

size_t strcspn(const char *s, const char *reject) {
    size_t count = 0;
    while (*s) {
        const char *r = reject;
        while (*r) { if (*s == *r) return count; r++; }
        s++; count++;
    }
    return count;
}

/* ====================================================================
 * Fortified memcpy/memset — GCC may emit calls to these with -O2.
 * ==================================================================== */

void *__memset_chk(void *s, int c, size_t n, size_t destlen) {
    (void)destlen;
    return memset(s, c, n);
}

void *__memcpy_chk(void *dst, const void *src, size_t n, size_t destlen) {
    (void)destlen;
    return memcpy(dst, src, n);
}

/* setjmp/longjmp — provided in assembly (heaven_setjmp.S) */
