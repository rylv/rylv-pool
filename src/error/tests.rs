use super::*;

use std::{
    cell::Cell,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

#[derive(Debug)]
struct FactoryFailure;

impl fmt::Display for FactoryFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("factory unavailable")
    }
}

impl Error for FactoryFailure {}

fn panic_error(payload: impl Any + Send) -> PanicError {
    catch_operation(|| std::panic::panic_any(payload))
        .err()
        .unwrap()
}

#[test]
fn catch_returns_the_original_success_value() {
    let value = Box::new(String::from("ready"));
    let original_address = std::ptr::from_ref(&*value).addr();
    let result = PoolError::<FactoryFailure>::catch(|| Ok(value)).unwrap();

    assert_eq!(std::ptr::from_ref(&*result).addr(), original_address);
    assert_eq!(*result, "ready");
}

#[test]
fn catch_returns_the_original_factory_error() {
    let failure = Box::new(42usize);
    let original_address = std::ptr::from_ref(&*failure).addr();
    let error = PoolError::catch(|| Err::<(), _>(failure)).err().unwrap();
    let PoolError::Factory(failure) = error else {
        panic!("expected the factory error");
    };

    assert_eq!(std::ptr::from_ref(&*failure).addr(), original_address);
    assert_eq!(*failure, 42);
}

#[test]
fn catch_invokes_a_consuming_operation_once() {
    let input = String::from("factory input");
    let mut calls = 0;
    let result = PoolError::<FactoryFailure>::catch(|| {
        calls += 1;
        Ok(input)
    })
    .unwrap();

    assert_eq!(result, "factory input");
    assert_eq!(calls, 1);
}

#[test]
fn catch_preserves_work_completed_before_a_panic_without_retrying() {
    let mut events = Vec::new();
    let error = PoolError::<FactoryFailure>::catch(|| -> Result<(), FactoryFailure> {
        events.push("completed");
        panic!("operation panic");
    })
    .err()
    .unwrap();

    assert_eq!(events, ["completed"]);
    let PoolError::Panic(error) = error else {
        panic!("expected an unwind error");
    };
    assert_eq!(error.to_string(), "operation panic");
}

#[test]
fn catch_accepts_exclusively_owned_mutable_operation_state() {
    let state = Cell::new(0);
    let result = PoolError::<FactoryFailure>::catch(|| {
        state.set(42);
        Ok(state.get())
    })
    .unwrap();

    assert_eq!(result, 42);
    assert_eq!(state.get(), 42);
}

#[test]
fn catch_operation_does_not_require_a_result_return_type() {
    let result = catch_operation(|| vec![1, 2, 3]).unwrap();
    assert_eq!(result, [1, 2, 3]);
}

#[test]
fn string_slice_payloads_keep_their_type_and_display_message() {
    for message in ["borrowed panic", ""] {
        let error = panic_error(message);
        assert_eq!(error.to_string(), message);
        assert_eq!(error.into_payload().downcast_ref::<&str>(), Some(&message));
    }
}

#[test]
fn owned_string_payloads_keep_their_type_and_display_message() {
    for message in ["owned panic", ""] {
        let error = panic_error(String::from(message));
        assert_eq!(error.to_string(), message);
        let payload = error.into_payload().downcast::<String>().unwrap();
        assert_eq!(*payload, message);
    }
}

#[test]
fn non_string_payloads_display_a_fallback_and_remain_recoverable() {
    let error = panic_error(Cell::new(42usize));

    assert_eq!(error.to_string(), "non-string panic payload");
    let payload = error.into_payload().downcast::<Cell<usize>>().unwrap();
    assert_eq!(payload.get(), 42);
}

#[test]
fn pool_error_display_accepts_borrowed_values_without_debug_or_error() {
    struct DisplayOnly<'a>(&'a str);

    impl fmt::Display for DisplayOnly<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    let message = String::from("borrowed failure");
    let error = PoolError::Factory(DisplayOnly(&message));

    assert_eq!(error.to_string(), "pool factory failed: borrowed failure");
}

#[test]
fn factory_error_display_debug_and_source_preserve_the_cause() {
    let error = PoolError::Factory(FactoryFailure);

    assert_eq!(
        error.to_string(),
        "pool factory failed: factory unavailable"
    );
    assert_eq!(format!("{error:?}"), "Factory(FactoryFailure)");
    let source = error.source().unwrap();
    assert!(source.downcast_ref::<FactoryFailure>().is_some());
    assert_eq!(source.to_string(), "factory unavailable");
    assert!(source.source().is_none());
}

#[test]
fn panic_error_display_debug_and_source_preserve_the_cause() {
    let error = PoolError::<FactoryFailure>::Panic(panic_error("root panic"));

    assert_eq!(error.to_string(), "pool operation panicked: root panic");
    let debug = format!("{error:?}");
    assert!(debug.starts_with("Panic(PanicError {"));
    assert!(debug.contains("root panic"));
    assert!(debug.contains(".."));
    let source = error.source().unwrap();
    assert!(source.downcast_ref::<PanicError>().is_some());
    assert_eq!(source.to_string(), "root panic");
    assert!(source.source().is_none());
}

#[test]
fn panic_error_debug_handles_all_payload_formats_without_consuming_them() {
    for error in [
        panic_error("borrowed message"),
        panic_error(String::from("owned message")),
        panic_error(42usize),
    ] {
        let message = error.to_string();
        let debug = format!("{error:?}");
        assert!(debug.starts_with("PanicError {"));
        assert!(debug.contains(&message));
        assert!(debug.ends_with(".. }"));
        assert_eq!(error.to_string(), message);
    }
}

struct PayloadDropProbe(Arc<AtomicUsize>);

impl Drop for PayloadDropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn payload_lives_until_the_recovered_payload_is_destroyed() {
    let drops = Arc::new(AtomicUsize::new(0));
    let error = panic_error(PayloadDropProbe(Arc::clone(&drops)));

    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(error.to_string(), "non-string panic payload");
    let payload = error.into_payload();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert!(payload.downcast_ref::<PayloadDropProbe>().is_some());
    drop(payload);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn dropping_the_error_destroys_an_unrecovered_payload_once() {
    let drops = Arc::new(AtomicUsize::new(0));
    let error =
        PoolError::<FactoryFailure>::Panic(panic_error(PayloadDropProbe(Arc::clone(&drops))));

    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(error);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn poisoning_the_payload_mutex_preserves_formatting_and_extraction() {
    let error = panic_error(String::from("original payload"));
    let poison = catch_unwind(AssertUnwindSafe(|| {
        let _payload = error.payload.lock().unwrap();
        panic!("poison test");
    }));

    assert!(poison.is_err());
    assert!(error.payload.is_poisoned());
    assert_eq!(error.to_string(), "original payload");
    assert!(format!("{error:?}").contains("original payload"));
    let payload = error.into_payload().downcast::<String>().unwrap();
    assert_eq!(*payload, "original payload");
}

struct FailingWriter;

impl fmt::Write for FailingWriter {
    fn write_str(&mut self, _text: &str) -> fmt::Result {
        Err(fmt::Error)
    }
}

#[test]
fn formatting_errors_are_propagated_without_consuming_the_payload() {
    let error = panic_error("still available");
    let mut writer = FailingWriter;

    assert!(fmt::Write::write_fmt(&mut writer, format_args!("{error}")).is_err());
    assert!(fmt::Write::write_fmt(&mut writer, format_args!("{error:?}")).is_err());
    assert_eq!(error.to_string(), "still available");
    assert_eq!(
        error.into_payload().downcast_ref::<&str>(),
        Some(&"still available")
    );
}

#[test]
fn panic_errors_are_send_and_sync_even_for_non_sync_payloads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PanicError>();
    assert_send_sync::<PoolError<std::io::Error>>();
    assert_send_sync::<PoolError<()>>();

    let error = Arc::new(panic_error(Cell::new(42usize)));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let shared = Arc::clone(&error);
            scope.spawn(move || {
                for _ in 0..8 {
                    assert_eq!(shared.to_string(), "non-string panic payload");
                    assert!(format!("{shared:?}").contains("non-string panic payload"));
                }
            });
        }
    });

    let error = Arc::try_unwrap(error).unwrap();
    let payload = error.into_payload().downcast::<Cell<usize>>().unwrap();
    assert_eq!(payload.get(), 42);
}

#[test]
fn recovered_non_sync_payload_can_move_to_another_thread() {
    let error = panic_error(Cell::new(42usize));
    let result = std::thread::spawn(move || {
        let payload = error.into_payload().downcast::<Cell<usize>>().unwrap();
        payload.set(payload.get() + 1);
        payload.get()
    })
    .join()
    .unwrap();

    assert_eq!(result, 43);
}
