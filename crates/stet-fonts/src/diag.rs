// stet - A PostScript Interpreter
// Copyright (c) 2026 Scott Bowman
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Diagnostics the interpreter notices while it runs and can do nothing about:
//! a page size it clamped, a font it could not embed, a band that failed to
//! render. They go to standard error by default, as a person running stet at a
//! terminal expects.
//!
//! An embedding program that speaks its own protocol on standard error (one
//! JSON object per line, say) installs a handler with [`set_handler`] and gets
//! each message as a string instead. The handler is process-wide, like
//! [`crate::host`]'s policy: install it once, before running any job.

use std::sync::RwLock;

/// Receives each diagnostic, without a trailing newline.
pub type Handler = Box<dyn Fn(&str) + Send + Sync>;

static HANDLER: RwLock<Option<Handler>> = RwLock::new(None);

/// Send diagnostics to `handler` instead of standard error; `None` restores
/// standard error.
pub fn set_handler(handler: Option<Handler>) {
    *HANDLER.write().unwrap_or_else(|e| e.into_inner()) = handler;
}

/// Report one diagnostic: to the installed handler, or to standard error.
/// Called through [`diag!`](crate::diag!).
pub fn emit(args: std::fmt::Arguments<'_>) {
    let guard = HANDLER.read().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(h) => h(&args.to_string()),
        None => eprintln!("{args}"),
    }
}

/// `eprintln!` for diagnostics: formats its arguments and hands them to
/// [`emit`].
#[macro_export]
macro_rules! diag {
    ($($arg:tt)*) => {
        $crate::diag::emit(format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_handler_receives_diagnostics() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        set_handler(Some(Box::new(move |m| {
            s.lock().unwrap().push(m.to_string())
        })));
        crate::diag!("page {} clamped", 3);
        set_handler(None);
        assert_eq!(seen.lock().unwrap().as_slice(), ["page 3 clamped"]);
    }
}
