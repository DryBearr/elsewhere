//! A JavaScript engine inside the app (QuickJS-ng, third_party/quickjs), for yt-dlp's
//! YouTube challenge solver. yt-dlp normally runs Deno, Node or QuickJS as a separate
//! program; iOS apps cannot start programs, so ytdlp.py hands the solver script to `run`
//! instead. The same code runs on every platform.

use std::ffi::{CStr, CString, c_char, c_int};
use std::thread;

unsafe extern "C" {
    fn elsewhere_js_run(
        code: *const c_char,
        len: usize,
        max_stack: usize,
        output: *mut *mut c_char,
    ) -> c_int;
    fn elsewhere_js_free(output: *mut c_char);
}

/// Stack for the engine's thread. QuickJS recurses deeply on YouTube's 2-3 MB player
/// script, and iOS gives new threads only 512 KB, so run on a thread with a big stack.
const STACK: usize = 64 << 20;
/// Leave QuickJS a margin below the real stack, so it reports "stack overflow" as a
/// JavaScript error instead of crashing.
const MAX_JS_STACK: usize = STACK - (4 << 20);

/// Runs `code` as a plain script in a fresh engine. Returns what it printed with
/// console.log (lines joined by `\n`), or the JavaScript error.
pub fn run(code: &str) -> Result<String, String> {
    let code = CString::new(code).map_err(|_| "the script contains a NUL byte".to_owned())?;
    thread::Builder::new()
        .name("javascript".to_owned())
        .stack_size(STACK)
        .spawn(move || {
            let mut output = std::ptr::null_mut();
            // SAFETY: `code` is NUL-terminated and outlives the call; `output` is set to
            // a malloc'd C string (never NULL on return), freed right below.
            let failed = unsafe {
                elsewhere_js_run(
                    code.as_ptr(),
                    code.as_bytes().len(),
                    MAX_JS_STACK,
                    &mut output,
                )
            };
            let text = if output.is_null() {
                String::new()
            } else {
                // SAFETY: as above; read it, then free it once.
                let text = unsafe { CStr::from_ptr(output) }
                    .to_string_lossy()
                    .into_owned();
                unsafe { elsewhere_js_free(output) };
                text
            };
            if failed == 0 { Ok(text) } else { Err(text) }
        })
        .map_err(|err| format!("could not start the JavaScript thread: {err}"))?
        .join()
        .map_err(|_| "the JavaScript engine crashed".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn prints_come_back_as_lines() {
        assert_eq!(
            run("console.log('a', 1); console.log(JSON.stringify({b: [2]}))").unwrap(),
            "a 1\n{\"b\":[2]}"
        );
    }

    #[test]
    fn errors_are_err_with_the_message() {
        let err = run("null.x").unwrap_err();
        assert!(err.contains("TypeError"), "{err}");
        assert!(run("let = ;").unwrap_err().contains("SyntaxError"));
        assert!(run("throw new Error('boom')").unwrap_err().contains("boom"));
    }

    #[test]
    fn deep_recursion_is_an_error_not_a_crash() {
        let err = run("function f(n) { return f(n + 1) + 1 } f(0)").unwrap_err();
        assert!(err.contains("stack") || err.contains("Range"), "{err}");
    }

    #[test]
    fn unicode_and_promises() {
        assert_eq!(run("console.log('გამარჯობა ✓')").unwrap(), "გამარჯობა ✓");
        assert_eq!(
            run("Promise.resolve(5).then(v => console.log(v))").unwrap(),
            "5"
        );
        assert_eq!(run("").unwrap(), "");
        assert!(run("a\0b").is_err());
    }
}
