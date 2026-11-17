use std::future::{Future, poll_fn};

/// Poll asynchronous work in `code`, restoring the polling thread's language before it yields.
/// Spawned tasks and blocking workers must independently inherit the current language at their boundary.
pub async fn scope_async<F: Future>(code: &str, future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    poll_fn(|context| super::scope(code, || future.as_mut().poll(context))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{current, scope};
    use std::{
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    async fn language_before_and_after_yield() -> (String, String) {
        let before = current().code.clone();
        let mut first = true;
        poll_fn(|context| {
            if std::mem::take(&mut first) {
                context.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await;
        (before, current().code.clone())
    }

    fn poll_once<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn interleaved_pending_futures_do_not_change_the_callers_language() {
        let mut french = Box::pin(scope_async("fr", language_before_and_after_yield()));
        let mut english = Box::pin(scope_async("en", language_before_and_after_yield()));
        scope("de", || {
            assert!(poll_once(french.as_mut()).is_pending());
            assert_eq!(current().code, "de");
            assert!(poll_once(english.as_mut()).is_pending());
            assert_eq!(current().code, "de");
            assert_eq!(poll_once(french.as_mut()), Poll::Ready(("fr".into(), "fr".into())));
            assert_eq!(current().code, "de");
            assert_eq!(poll_once(english.as_mut()), Poll::Ready(("en".into(), "en".into())));
            assert_eq!(current().code, "de");
        });
        assert_eq!(current().code, "en");
    }

    #[test]
    fn a_pending_future_keeps_its_language_after_moving_to_another_thread() {
        let future = Box::pin(scope_async("fr", language_before_and_after_yield()));
        let mut future = std::thread::spawn(move || {
            let mut future = future;
            scope("de", || {
                assert!(poll_once(future.as_mut()).is_pending());
                assert_eq!(current().code, "de");
            });
            future
        })
        .join()
        .unwrap();
        let result = std::thread::spawn(move || {
            scope("es", || {
                let result = poll_once(future.as_mut());
                assert_eq!(current().code, "es");
                result
            })
        })
        .join()
        .unwrap();
        assert_eq!(result, Poll::Ready(("fr".into(), "fr".into())));
    }
}
