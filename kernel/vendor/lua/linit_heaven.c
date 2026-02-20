/*
 * linit_heaven.c — Filtered library init for OSqlite bare-metal Lua.
 *
 * Only loads libraries that work without an OS:
 * base, table, string, math, coroutine, utf8.
 * Excludes: io, os, package, debug, loadlib.
 */
#define linit_c
#define LUA_LIB

#include "lprefix.h"
#include "lua.h"
#include "lualib.h"
#include "lauxlib.h"

static const luaL_Reg loadedlibs[] = {
    {LUA_GNAME,        luaopen_base},
    {LUA_TABLIBNAME,   luaopen_table},
    {LUA_STRLIBNAME,   luaopen_string},
    {LUA_MATHLIBNAME,  luaopen_math},
    {LUA_COLIBNAME,    luaopen_coroutine},
    {LUA_UTF8LIBNAME,  luaopen_utf8},
    {NULL, NULL}
};

LUALIB_API void luaL_openlibs (lua_State *L) {
    const luaL_Reg *lib;
    for (lib = loadedlibs; lib->func; lib++) {
        luaL_requiref(L, lib->name, lib->func, 1);
        lua_pop(L, 1);
    }
}
