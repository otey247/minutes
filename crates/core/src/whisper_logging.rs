//! Keep Whisper's C logs visible while excluding the error emitted for an
//! intentionally aborted streaming pass. ggml retains whisper-rs's own hook.

use std::cell::RefCell;
use std::ffi::{c_char, c_void, CStr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Once,
};
use whisper_rs::whisper_rs_sys::ggml_log_level;
use whisper_rs::GGMLLogLevel;

const CANCELED_ENCODE_LOG: &str = "whisper_full_with_state: failed to encode";
const CANCELED_DECODE_LOG: &str = "whisper_full_with_state: failed to decode";

thread_local! {
    // whisper_full_with_state emits its encode/decode error on the thread that called
    // WhisperState::full. The abort callback may run on a backend worker, so
    // its result is shared with that thread through an atomic flag.
    static ACTIVE_ABORT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Scope an inference pass's C-level logs to the abort callback that belongs
/// to it. Restoring the previous marker also makes nested calls harmless.
pub(crate) struct AbortLogScope {
    previous: Option<Arc<AtomicBool>>,
}

impl AbortLogScope {
    pub(crate) fn enter(abort_fired: Arc<AtomicBool>) -> Self {
        let previous = ACTIVE_ABORT.with(|active| active.replace(Some(abort_fired)));
        Self { previous }
    }
}

impl Drop for AbortLogScope {
    fn drop(&mut self) {
        ACTIVE_ABORT.with(|active| {
            active.replace(self.previous.take());
        });
    }
}

pub(crate) fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        whisper_rs::install_logging_hooks();
        // whisper-rs exposes this C callback setter. Replace only its Whisper
        // callback; its ggml hook continues to forward backend diagnostics.
        // SAFETY: the callback has a static lifetime and never uses user_data.
        unsafe { whisper_rs::set_log_callback(Some(whisper_log), std::ptr::null_mut()) };
    });
}

unsafe extern "C" fn whisper_log(level: ggml_log_level, text: *const c_char, _: *mut c_void) {
    if text.is_null() {
        tracing::error!(
            target: "whisper_rs::whisper_logging_hook",
            "whisper log callback received null text"
        );
        return;
    }
    // SAFETY: whisper.cpp supplies a NUL-terminated string for this callback.
    let message = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let level = GGMLLogLevel::from(level);
    if is_expected_abort_log(&level, &message) {
        return;
    }
    forward_log(level, message.trim());
}

fn is_expected_abort_log(level: &GGMLLogLevel, message: &str) -> bool {
    matches!(level, GGMLLogLevel::Error)
        && (message.trim() == CANCELED_ENCODE_LOG || message.trim() == CANCELED_DECODE_LOG)
        && ACTIVE_ABORT.with(|active| {
            active
                .borrow()
                .as_ref()
                .is_some_and(|fired| fired.load(Ordering::Acquire))
        })
}

fn forward_log(level: GGMLLogLevel, message: &str) {
    // Preserve the original hook's target so host EnvFilters continue to
    // handle Whisper INFO/DEBUG messages exactly as before.
    match level {
        GGMLLogLevel::None | GGMLLogLevel::Cont => {
            tracing::trace!(target: "whisper_rs::whisper_logging_hook", "{message}")
        }
        GGMLLogLevel::Info => {
            tracing::info!(target: "whisper_rs::whisper_logging_hook", "{message}")
        }
        GGMLLogLevel::Warn => {
            tracing::warn!(target: "whisper_rs::whisper_logging_hook", "{message}")
        }
        GGMLLogLevel::Error => {
            tracing::error!(target: "whisper_rs::whisper_logging_hook", "{message}")
        }
        GGMLLogLevel::Debug => {
            tracing::debug!(target: "whisper_rs::whisper_logging_hook", "{message}")
        }
        GGMLLogLevel::Unknown(raw_level) => tracing::warn!(
            target: "whisper_rs::whisper_logging_hook",
            "unknown Whisper log level {raw_level}: {message}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::io::{self, Write};
    use std::sync::Mutex;

    struct CapturedLog(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn callback_keeps_real_errors_visible() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let captured = Arc::clone(&captured);
                move || CapturedLog(Arc::clone(&captured))
            })
            .with_ansi(false)
            .without_time()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let aborted = Arc::new(AtomicBool::new(false));
            let _scope = AbortLogScope::enter(Arc::clone(&aborted));
            let emit = |message: &str| {
                let message = CString::new(message).unwrap();
                // SAFETY: the callback reads this NUL-terminated string only
                // during the call and ignores its user-data pointer.
                unsafe {
                    whisper_log(
                        whisper_rs::whisper_rs_sys::ggml_log_level_GGML_LOG_LEVEL_ERROR,
                        message.as_ptr(),
                        std::ptr::null_mut(),
                    )
                };
            };
            emit(CANCELED_ENCODE_LOG); // Real failure: abort never fired.
            emit(CANCELED_DECODE_LOG); // Real failure: abort never fired.
            aborted.store(true, Ordering::Release);
            emit(CANCELED_ENCODE_LOG); // Expected cancellation: hidden.
            emit(CANCELED_DECODE_LOG); // Expected cancellation: hidden.
            emit("an unrelated whisper error"); // Unrelated error: visible.
        });
        let output = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert_eq!(output.matches(CANCELED_ENCODE_LOG).count(), 1);
        assert_eq!(output.matches(CANCELED_DECODE_LOG).count(), 1);
        assert!(output.contains("an unrelated whisper error"));
    }

    #[test]
    fn only_an_abort_from_the_active_pass_suppresses_exact_encode_and_decode_errors() {
        let aborted = Arc::new(AtomicBool::new(false));
        assert!(!is_expected_abort_log(
            &GGMLLogLevel::Error,
            CANCELED_ENCODE_LOG
        ));

        {
            let _scope = AbortLogScope::enter(Arc::clone(&aborted));
            assert!(!is_expected_abort_log(
                &GGMLLogLevel::Error,
                CANCELED_ENCODE_LOG
            ));
            assert!(!is_expected_abort_log(
                &GGMLLogLevel::Error,
                CANCELED_DECODE_LOG
            ));
            aborted.store(true, Ordering::Release);
            assert!(is_expected_abort_log(
                &GGMLLogLevel::Error,
                CANCELED_ENCODE_LOG
            ));
            assert!(is_expected_abort_log(
                &GGMLLogLevel::Error,
                CANCELED_DECODE_LOG
            ));
            assert!(!is_expected_abort_log(
                &GGMLLogLevel::Warn,
                CANCELED_ENCODE_LOG
            ));
            assert!(!is_expected_abort_log(
                &GGMLLogLevel::Error,
                "whisper_full_with_state: failed to decode other"
            ));
            assert!(!std::thread::spawn(|| {
                is_expected_abort_log(&GGMLLogLevel::Error, CANCELED_ENCODE_LOG)
                    || is_expected_abort_log(&GGMLLogLevel::Error, CANCELED_DECODE_LOG)
            })
            .join()
            .unwrap());
        }

        assert!(!is_expected_abort_log(
            &GGMLLogLevel::Error,
            CANCELED_ENCODE_LOG
        ));
        assert!(!is_expected_abort_log(
            &GGMLLogLevel::Error,
            CANCELED_DECODE_LOG
        ));
    }

    #[test]
    fn nested_pass_restores_its_predecessor() {
        let outer = Arc::new(AtomicBool::new(true));
        let inner = Arc::new(AtomicBool::new(false));
        let _outer = AbortLogScope::enter(outer);
        {
            let _inner = AbortLogScope::enter(inner);
            assert!(!is_expected_abort_log(
                &GGMLLogLevel::Error,
                CANCELED_ENCODE_LOG
            ));
        }
        assert!(is_expected_abort_log(
            &GGMLLogLevel::Error,
            CANCELED_ENCODE_LOG
        ));
    }
}
