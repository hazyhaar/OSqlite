/* luaconf_heaven.h — OSqlite bare-metal overrides for Lua 5.5.0 */

#ifndef LUACONF_HEAVEN_H
#define LUACONF_HEAVEN_H

/* ========== I/O — redirect to serial console ========== */
extern void heaven_serial_write(const char *s, int len);
extern int  heaven_strlen(const char *s);
extern int  heaven_snprintf(char *buf, int count, const char *fmt, ...);

#define lua_writestring(s, l)   heaven_serial_write(s, l)
#define lua_writeline()         heaven_serial_write("\n", 1)

#define lua_writestringerror(s, p) \
    do { char _buf[256]; \
         heaven_snprintf(_buf, sizeof(_buf), s, p); \
         heaven_serial_write(_buf, heaven_strlen(_buf)); \
    } while(0)

/* ========== Disable OS-dependent features ========== */
#undef LUA_USE_POSIX
#undef LUA_USE_DLOPEN
#undef LUA_USE_READLINE
#undef LUA_USE_C89

/* ========== Numbers — keep double + int64 (default) ========== */
/* The kernel has FPU/SSE enabled. */

#endif /* LUACONF_HEAVEN_H */
