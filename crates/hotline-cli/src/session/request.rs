//! A request to the server that runs off the event loop.

use std::fmt::Display;
use std::future::Future;

use futures::future::BoxFuture;

use super::ClientSession;
use crate::line::LineKind;

/// What a reply does to the session.
type Apply<T> = Box<dyn FnOnce(&mut ClientSession) -> Next<T> + Send>;

/// The result of a reply: a value, or the next request of a chain.
pub enum Next<T> {
    Done(T),
    Then(Request<T>),
}

/// A request whose future needs no session. Its reply then changes the
/// session and gives a `T`.
#[must_use]
pub struct Request<T>(BoxFuture<'static, Apply<T>>);

impl<T: 'static> Request<T> {
    /// Wait for `future`, then apply its output to the session.
    pub(super) fn new<F, A>(future: F) -> Self
    where
        F: Future<Output = A> + Send + 'static,
        A: FnOnce(&mut ClientSession) -> T + Send + 'static,
    {
        Self(Box::pin(async move {
            let apply = future.await;
            Box::new(move |session: &mut ClientSession| Next::Done(apply(session))) as Apply<T>
        }))
    }

    /// Wait for `future`, then give its value to `ok`. A failure shows
    /// `failed` and the error as an error line, and gives `T::default()`.
    pub(super) fn report<V, E>(
        future: impl Future<Output = Result<V, E>> + Send + 'static,
        failed: impl Display + Send + 'static,
        ok: impl FnOnce(&mut ClientSession, V) -> T + Send + 'static,
    ) -> Self
    where
        T: Default,
        V: Send + 'static,
        E: Display + Send + 'static,
    {
        Self::new(async move {
            let result = future.await;
            move |session: &mut ClientSession| match result {
                Ok(value) => ok(session, value),
                Err(e) => {
                    session.push(format!("{failed}: {e}"), LineKind::Error);
                    T::default()
                }
            }
        })
    }

    /// Wait for `future`, and give its output.
    pub(super) fn wait<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
        T: Send,
    {
        Self::new(async move {
            let value = future.await;
            move |_: &mut ClientSession| value
        })
    }

    /// A request that sends nothing and gives `value`.
    pub(super) fn ready(value: T) -> Self
    where
        T: Send,
    {
        Self::wait(async move { value })
    }

    /// After this reply, run the request that `next` makes from its value.
    pub fn and_then<U: 'static>(
        self,
        next: impl FnOnce(&mut ClientSession, T) -> Request<U> + Send + 'static,
    ) -> Request<U> {
        Request(Box::pin(async move {
            let apply = self.0.await;
            Box::new(move |session: &mut ClientSession| match apply(session) {
                Next::Done(value) => Next::Then(next(session, value)),
                Next::Then(request) => Next::Then(request.and_then(next)),
            }) as Apply<U>
        }))
    }

    /// After this reply, change its value with `f`.
    pub fn map<U: 'static>(
        self,
        f: impl FnOnce(&mut ClientSession, T) -> U + Send + 'static,
    ) -> Request<U> {
        Request(Box::pin(async move {
            let apply = self.0.await;
            Box::new(move |session: &mut ClientSession| match apply(session) {
                Next::Done(value) => Next::Done(f(session, value)),
                Next::Then(request) => Next::Then(request.map(f)),
            }) as Apply<U>
        }))
    }

    /// Wait for the reply. The caller applies it to the session.
    pub async fn reply(self) -> impl FnOnce(&mut ClientSession) -> Next<T> + Send {
        self.0.await
    }

    /// Run the request and each request after it on `session`.
    #[cfg(test)]
    pub async fn run(mut self, session: &mut ClientSession) -> T {
        loop {
            match (self.0.await)(session) {
                Next::Done(value) => return value,
                Next::Then(request) => self = request,
            }
        }
    }
}
