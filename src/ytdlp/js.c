// Runs one JavaScript program in a fresh QuickJS context and returns what it printed with
// console.log, joined by newlines. Used by js.rs; see there.

#include <stdlib.h>
#include <string.h>

#include "quickjs.h"

// console.log collects lines; the other console methods are ignored.
static const char PRELUDE[] =
    "globalThis.__elsewhere_out = [];"
    "globalThis.console = {"
    "  log: (...args) => { __elsewhere_out.push(args.join(' ')); },"
    "  info() {}, warn() {}, error() {}, debug() {}, trace() {},"
    "};";

static const char RESULT[] = "__elsewhere_out.join('\\n')";

static char *copy_text(const char *text, size_t len) {
    char *copy = malloc(len + 1);
    if (copy) {
        memcpy(copy, text, len);
        copy[len] = '\0';
    }
    return copy;
}

// Converts a JS value to a malloc'd UTF-8 string, or NULL.
static char *value_text(JSContext *ctx, JSValueConst value) {
    size_t len;
    const char *text = JS_ToCStringLen(ctx, &len, value);
    if (!text) {
        return NULL;
    }
    char *copy = copy_text(text, len);
    JS_FreeCString(ctx, text);
    return copy;
}

// Takes the pending exception as text, with its stack trace when there is one.
static char *exception_text(JSContext *ctx) {
    JSValue error = JS_GetException(ctx);
    char *text = value_text(ctx, error);
    JS_FreeValue(ctx, error);
    return text ? text : copy_text("JavaScript error", 16);
}

// `code` must have a NUL byte at code[len], as QuickJS requires. Returns 0 with the printed
// text in *output, or 1 with the error in *output. Free *output with elsewhere_js_free.
int elsewhere_js_run(const char *code, size_t len, size_t max_stack, char **output) {
    *output = NULL;
    JSRuntime *rt = JS_NewRuntime();
    if (!rt) {
        *output = copy_text("out of memory", 13);
        return 1;
    }
    JS_SetMaxStackSize(rt, max_stack);
    JSContext *ctx = JS_NewContext(rt);
    if (!ctx) {
        JS_FreeRuntime(rt);
        *output = copy_text("out of memory", 13);
        return 1;
    }

    int failed = 1;
    JSValue result = JS_Eval(ctx, PRELUDE, sizeof PRELUDE - 1, "<prelude>", JS_EVAL_TYPE_GLOBAL);
    if (JS_IsException(result)) {
        *output = exception_text(ctx);
    } else {
        JS_FreeValue(ctx, result);
        result = JS_Eval(ctx, code, len, "<script>", JS_EVAL_TYPE_GLOBAL);
        if (JS_IsException(result)) {
            *output = exception_text(ctx);
        } else {
            JS_FreeValue(ctx, result);
            // Run any promise jobs the script queued, so their console.log output counts.
            JSContext *job_ctx;
            int job;
            while ((job = JS_ExecutePendingJob(rt, &job_ctx)) > 0) {
            }
            if (job < 0) {
                *output = exception_text(job_ctx);
            } else {
                result = JS_Eval(ctx, RESULT, sizeof RESULT - 1, "<result>", JS_EVAL_TYPE_GLOBAL);
                if (JS_IsException(result)) {
                    *output = exception_text(ctx);
                } else {
                    *output = value_text(ctx, result);
                    failed = *output == NULL;
                    JS_FreeValue(ctx, result);
                }
            }
        }
    }

    JS_FreeContext(ctx);
    JS_FreeRuntime(rt);
    if (!*output) {
        *output = copy_text("out of memory", 13);
        failed = 1;
    }
    return failed;
}

void elsewhere_js_free(char *output) {
    free(output);
}
