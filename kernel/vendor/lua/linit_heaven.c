/*
 * linit_heaven.c — Filtered library init for OSqlite bare-metal Lua 5.5.
 *
 * Only loads libraries that work without an OS:
 * base, table, string, math, coroutine, utf8.
 * Excludes: io, os, package, debug, loadlib.
 *
 * Lua 5.5 changed luaL_openlibs to a macro that calls
 * luaL_openselectedlibs, so we define that function here.
 */
#define linit_c
#define LUA_LIB

#include "lprefix.h"

#include <stddef.h>

#include "lua.h"
#include "lualib.h"
#include "lauxlib.h"
#include "llimits.h"

LUALIB_API void luaL_openselectedlibs (lua_State *L, int load, int preload) {
    /* We ignore the load/preload bitmasks on bare metal — always load
       the same safe subset of libraries. */
    static const luaL_Reg loadedlibs[] = {
        {LUA_GNAME,        luaopen_base},
        {LUA_TABLIBNAME,   luaopen_table},
        {LUA_STRLIBNAME,   luaopen_string},
        {LUA_MATHLIBNAME,  luaopen_math},
        {LUA_COLIBNAME,    luaopen_coroutine},
        {LUA_UTF8LIBNAME,  luaopen_utf8},
        {NULL, NULL}
    };
    const luaL_Reg *lib;
    for (lib = loadedlibs; lib->func; lib++) {
        luaL_requiref(L, lib->name, lib->func, 1);
        lua_pop(L, 1);
    }
    (void)load;
    (void)preload;
}
