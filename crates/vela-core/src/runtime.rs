//! The asynchronous seam between core and its host.
//!
//! The original runs on the browser's event loop: promises, `setTimeout`,
//! `fetch`, `WebSocket`, `AbortSignal`. Core stays platform-neutral, so it
//! asks a [`Platform`] for those services instead. `vela-web` implements the
//! trait on the browser; [`testing::ManualPlatform`] is a deterministic
//! implementation with a hand-driven clock for tests.
//!
//! Everything here is single-threaded, like the event loop it stands in for:
//! futures are not `Send`, shared state is `Rc<RefCell<…>>`.

use std::cell::{Cell, RefCell};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use crate::data_unavailable::DataUnavailableError;

/// A boxed future that need not be `Send`.
pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Detaches a previously registered subscription (the original's `() => void`).
pub type Unsubscribe = Box<dyn FnOnce()>;

/// An [`Unsubscribe`] that does nothing.
pub fn noop_unsubscribe() -> Unsubscribe {
    Box::new(|| {})
}

/// Why a data request failed.
#[derive(Clone, Debug, PartialEq)]
pub enum DataError {
    /// The source refuses the series for good (the original's `DataUnavailableError`).
    Unavailable(Box<DataUnavailableError>),
    /// Anything else: a network failure, a malformed answer. Callers log it and
    /// read it as an empty answer, as the original does with a thrown error.
    Other(String),
}

impl DataError {
    pub fn other(message: impl Into<String>) -> Self {
        DataError::Other(message.into())
    }
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataError::Unavailable(e) => write!(f, "{e}"),
            DataError::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for DataError {}

impl From<DataUnavailableError> for DataError {
    fn from(e: DataUnavailableError) -> Self {
        DataError::Unavailable(Box::new(e))
    }
}

impl From<NetError> for DataError {
    fn from(e: NetError) -> Self {
        DataError::Other(e.to_string())
    }
}

// ── cancellation ──

#[derive(Default)]
struct AbortState {
    aborted: Cell<bool>,
    listeners: RefCell<Vec<Box<dyn FnOnce()>>>,
}

/// The original's `AbortController`: owns the right to abort.
#[derive(Clone, Default)]
pub struct AbortController {
    state: Rc<AbortState>,
}

impl AbortController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn signal(&self) -> AbortSignal {
        AbortSignal {
            state: self.state.clone(),
        }
    }

    /// Abort: later calls do nothing. Listeners run once, in registration order.
    pub fn abort(&self) {
        if self.state.aborted.replace(true) {
            return;
        }
        let listeners = std::mem::take(&mut *self.state.listeners.borrow_mut());
        for listener in listeners {
            listener();
        }
    }
}

/// The original's `AbortSignal`: lets work observe an abort.
#[derive(Clone)]
pub struct AbortSignal {
    state: Rc<AbortState>,
}

impl AbortSignal {
    pub fn aborted(&self) -> bool {
        self.state.aborted.get()
    }

    /// Run `listener` when the signal aborts; at once if it already has.
    pub fn on_abort(&self, listener: impl FnOnce() + 'static) {
        if self.aborted() {
            listener();
        } else {
            self.state.listeners.borrow_mut().push(Box::new(listener));
        }
    }
}

impl fmt::Debug for AbortSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortSignal")
            .field("aborted", &self.aborted())
            .finish()
    }
}

// ── a one-shot value, the bridge from callbacks to futures ──

struct OneshotState<T> {
    value: Option<T>,
    waker: Option<Waker>,
}

/// The sending half of [`oneshot`].
pub struct OneshotSender<T> {
    state: Rc<RefCell<OneshotState<T>>>,
}

impl<T> OneshotSender<T> {
    /// Resolve the paired future. A second value replaces an unread first one.
    pub fn send(&self, value: T) {
        let waker = {
            let mut state = self.state.borrow_mut();
            state.value = Some(value);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> Clone for OneshotSender<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

/// The receiving half of [`oneshot`]: a future resolving to the sent value.
/// It stays pending forever if nothing is sent.
pub struct OneshotReceiver<T> {
    state: Rc<RefCell<OneshotState<T>>>,
}

impl<T> Future for OneshotReceiver<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let mut state = self.state.borrow_mut();
        match state.value.take() {
            Some(value) => Poll::Ready(value),
            None => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// A promise in two halves: a callback resolves it, a future awaits it.
pub fn oneshot<T>() -> (OneshotSender<T>, OneshotReceiver<T>) {
    let state = Rc::new(RefCell::new(OneshotState {
        value: None,
        waker: None,
    }));
    (
        OneshotSender {
            state: state.clone(),
        },
        OneshotReceiver { state },
    )
}

// ── network ──

/// A network-level failure: the request never produced a response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetError {
    /// The request was aborted through its signal.
    Aborted,
    /// The request ran past its timeout.
    Timeout,
    /// The transport failed (DNS, connection, CORS, …).
    Failed(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Aborted => f.write_str("request aborted"),
            NetError::Timeout => f.write_str("request timed out"),
            NetError::Failed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for NetError {}

/// One `fetch` call.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub url: String,
    /// `GET` unless set.
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Abort the request after this long (the original's `AbortSignal.timeout`).
    pub timeout_ms: Option<f64>,
    pub signal: Option<AbortSignal>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: "GET".to_string(),
            headers: Vec::new(),
            body: None,
            timeout_ms: None,
            signal: None,
        }
    }

    pub fn post_json(url: impl Into<String>, body: impl Into<String>) -> Self {
        let mut request = Self::get(url);
        request.method = "POST".to_string();
        request
            .headers
            .push(("Content-Type".to_string(), "application/json".to_string()));
        request.body = Some(body.into());
        request
    }

    pub fn timeout(mut self, ms: f64) -> Self {
        self.timeout_ms = Some(ms);
        self
    }

    pub fn signal(mut self, signal: AbortSignal) -> Self {
        self.signal = Some(signal);
        self
    }
}

/// What a `fetch` resolved to. A non-2xx status is a response, not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    /// The original's `response.ok`: a 2xx status.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The body parsed as JSON.
    pub fn json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.body)
    }
}

/// A WebSocket text-frame handler.
pub type MessageHandler = Box<dyn FnMut(&str)>;

/// What a `fetch` resolves to.
pub type FetchResult = Result<HttpResponse, NetError>;

/// The callbacks of one WebSocket (`onopen`, `onmessage`, `onerror`, `onclose`).
#[derive(Default)]
pub struct SocketHandlers {
    pub on_open: Option<Box<dyn FnMut()>>,
    /// Text frames only; the providers speak JSON.
    pub on_message: Option<MessageHandler>,
    pub on_error: Option<Box<dyn FnMut()>>,
    pub on_close: Option<Box<dyn FnMut()>>,
}

/// An open (or opening) WebSocket.
pub trait Socket {
    /// Send a text frame. Dropped silently while the socket is not open.
    fn send(&self, text: &str);
    /// Close the socket. The `on_close` handler still fires, as in the browser.
    fn close(&self);
    /// The original's `readyState === WebSocket.OPEN`.
    fn is_open(&self) -> bool;
}

/// A scheduled timer, for [`Platform::clear_timeout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimerId(pub u64);

/// The host services core's asynchronous code runs on.
pub trait Platform {
    /// Wall-clock now, in epoch ms (`Date.now()`).
    fn now_ms(&self) -> f64;
    /// Run a future to completion in the background (the original's `void promise`).
    fn spawn(&self, future: LocalBoxFuture<'static, ()>);
    /// `setTimeout`: run `callback` once after `ms`.
    fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> TimerId;
    /// `clearTimeout`. Clearing a fired or unknown timer does nothing.
    fn clear_timeout(&self, id: TimerId);
    /// `fetch`. An abort or timeout resolves to the matching [`NetError`].
    fn fetch(
        &self,
        request: HttpRequest,
    ) -> LocalBoxFuture<'static, Result<HttpResponse, NetError>>;
    /// `new WebSocket(url)`.
    fn open_socket(&self, url: &str, handlers: SocketHandlers) -> Rc<dyn Socket>;
    /// `console.warn` / `console.error`: a diagnostic for the developer.
    fn warn(&self, message: &str);
    /// `Math.random()`: a value in `[0, 1)`.
    fn random(&self) -> f64;
}

/// A future resolving after `ms` on the platform's clock
/// (`new Promise((resolve) => setTimeout(resolve, ms))`).
pub fn sleep(platform: &dyn Platform, ms: f64) -> OneshotReceiver<()> {
    let (tx, rx) = oneshot();
    platform.set_timeout(ms, Box::new(move || tx.send(())));
    rx
}

// ── turns ──

/// A future that is `Pending` exactly once, after waking its own waker, then
/// `Ready`: the executor gets one chance to run everything else that is
/// runnable before the awaiting code continues. See [`yield_now`].
#[derive(Debug, Default)]
pub struct YieldNow {
    yielded: bool,
}

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

/// Give up the current turn: the code after `yield_now().await` runs in a
/// later executor pass, never in the same poll as the code before it.
///
/// Both executors the core runs on re-poll a task that woke itself while
/// pending: [`testing::ManualPlatform::run`] makes another pass, and
/// `wasm_bindgen_futures::spawn_local` queues the task again as a microtask.
pub fn yield_now() -> YieldNow {
    YieldNow::default()
}

/// `await future` the way JavaScript does it: wait for `future`, then give up
/// the turn before continuing.
///
/// A Rust `.await` on a ready future keeps running in the same poll, so a whole
/// chain of ready futures completes inside the call that started it. JavaScript
/// never does that: `await p` resumes in a later microtask even when `p` is
/// settled, and an `async` function called without `await` hands control back
/// to its caller at its first `await`. Code written against that behavior (a
/// constructor that starts a load, with the caller subscribing right after;
/// state read after a `void asyncCall()`) needs this helper at every `await`
/// of the original.
///
/// Reproduced exactly: the code after the await never runs in the same poll as
/// the code before it, and a chain of nested awaits costs one turn per level,
/// as nested promises do. Approximated: JavaScript resumes awaiters in
/// microtask FIFO order across the whole program, while a Rust executor
/// re-polls woken tasks in its own order (`ManualPlatform`: task-list order,
/// one pass per turn; the browser: the order the wakers fired). A promise
/// resolved from a callback resumes its awaiter one turn after the resolution
/// in JavaScript; here it takes two (the wake, then the yield). Two tasks that
/// become runnable in the same turn may therefore interleave differently from
/// the original; nothing in the port relies on that order.
pub async fn js_await<F: Future>(future: F) -> F::Output {
    let value = future.await;
    yield_now().await;
    value
}

pub mod testing {
    //! A deterministic [`Platform`] for tests: time moves only when the test
    //! says so, the network answers from a script, and sockets are driven by
    //! hand.

    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Wake;

    use super::*;

    struct Flag(AtomicBool);

    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    type Responder = Box<dyn FnMut(&HttpRequest) -> Option<Result<HttpResponse, NetError>>>;

    struct Timer {
        id: u64,
        due: f64,
        callback: Box<dyn FnOnce()>,
    }

    /// One socket the code under test opened.
    pub struct FakeSocket {
        pub url: String,
        handlers: RefCell<SocketHandlers>,
        open: Cell<bool>,
        closed: Cell<bool>,
        sent: RefCell<Vec<String>>,
    }

    impl FakeSocket {
        fn call(
            &self,
            pick: impl FnOnce(&mut SocketHandlers) -> Option<Box<dyn FnMut()>>,
            put: impl FnOnce(&mut SocketHandlers, Box<dyn FnMut()>),
        ) {
            // Take the handler out while it runs, so it may touch the socket.
            let handler = pick(&mut self.handlers.borrow_mut());
            if let Some(mut handler) = handler {
                handler();
                put(&mut self.handlers.borrow_mut(), handler);
            }
        }

        /// The server accepted the connection.
        pub fn emit_open(&self) {
            self.open.set(true);
            self.call(|h| h.on_open.take(), |h, f| h.on_open = Some(f));
        }

        /// The server sent a text frame.
        pub fn emit_message(&self, text: &str) {
            let handler = self.handlers.borrow_mut().on_message.take();
            if let Some(mut handler) = handler {
                handler(text);
                self.handlers.borrow_mut().on_message = Some(handler);
            }
        }

        pub fn emit_error(&self) {
            self.call(|h| h.on_error.take(), |h, f| h.on_error = Some(f));
        }

        /// The connection ended (from either side).
        pub fn emit_close(&self) {
            if self.closed.replace(true) {
                return;
            }
            self.open.set(false);
            self.call(|h| h.on_close.take(), |h, f| h.on_close = Some(f));
        }

        /// The text frames the code under test sent while open.
        pub fn sent(&self) -> Vec<String> {
            self.sent.borrow().clone()
        }

        /// Whether the code under test closed the socket (or it was closed by hand).
        pub fn is_closed(&self) -> bool {
            self.closed.get()
        }
    }

    impl Socket for FakeSocket {
        fn send(&self, text: &str) {
            if self.open.get() {
                self.sent.borrow_mut().push(text.to_string());
            }
        }

        fn close(&self) {
            self.emit_close();
        }

        fn is_open(&self) -> bool {
            self.open.get()
        }
    }

    #[derive(Default)]
    struct Inner {
        now: Cell<f64>,
        next_timer: Cell<u64>,
        timers: RefCell<Vec<Timer>>,
        tasks: RefCell<Vec<LocalBoxFuture<'static, ()>>>,
        spawned: RefCell<VecDeque<LocalBoxFuture<'static, ()>>>,
        responder: RefCell<Option<Responder>>,
        requests: RefCell<Vec<HttpRequest>>,
        pending: RefCell<Vec<(HttpRequest, OneshotSender<FetchResult>)>>,
        random_state: Cell<u64>,
        sockets: RefCell<Vec<Rc<FakeSocket>>>,
        warnings: RefCell<Vec<String>>,
    }

    /// A [`Platform`] driven by the test. Clone it freely: clones share state.
    #[derive(Clone, Default)]
    pub struct ManualPlatform {
        inner: Rc<Inner>,
    }

    impl ManualPlatform {
        /// A platform whose clock starts at `now_ms`.
        pub fn new(now_ms: f64) -> Self {
            let platform = Self::default();
            platform.inner.now.set(now_ms);
            platform
        }

        /// This platform as the trait object core code takes.
        pub fn handle(&self) -> Rc<dyn Platform> {
            Rc::new(self.clone())
        }

        /// Answer requests from `responder`. Returning `None` leaves a request
        /// pending until [`respond`](Self::respond) or an abort.
        pub fn on_fetch(
            &self,
            responder: impl FnMut(&HttpRequest) -> Option<Result<HttpResponse, NetError>> + 'static,
        ) {
            *self.inner.responder.borrow_mut() = Some(Box::new(responder));
        }

        /// Answer every request to a URL containing `fragment` with a 200 and `body`.
        pub fn serve(&self, routes: Vec<(&'static str, String)>) {
            self.on_fetch(move |request| {
                routes
                    .iter()
                    .find(|(fragment, _)| request.url.contains(fragment))
                    .map(|(_, body)| {
                        Ok(HttpResponse {
                            status: 200,
                            headers: Vec::new(),
                            body: body.clone(),
                        })
                    })
            });
        }

        /// Every request made so far, in order.
        pub fn requests(&self) -> Vec<HttpRequest> {
            self.inner.requests.borrow().clone()
        }

        /// How many requests are still waiting for an answer.
        pub fn pending_requests(&self) -> usize {
            self.inner.pending.borrow().len()
        }

        /// Answer the oldest pending request whose URL contains `fragment`.
        /// Returns whether one was waiting.
        pub fn respond(&self, fragment: &str, response: Result<HttpResponse, NetError>) -> bool {
            let found = {
                let mut pending = self.inner.pending.borrow_mut();
                pending
                    .iter()
                    .position(|(request, _)| request.url.contains(fragment))
                    .map(|i| pending.remove(i))
            };
            match found {
                Some((_, tx)) => {
                    tx.send(response);
                    self.run();
                    true
                }
                None => false,
            }
        }

        /// The sockets opened so far, in order.
        pub fn sockets(&self) -> Vec<Rc<FakeSocket>> {
            self.inner.sockets.borrow().clone()
        }

        pub fn warnings(&self) -> Vec<String> {
            self.inner.warnings.borrow().clone()
        }

        /// How many timers are scheduled and not yet fired or cleared.
        pub fn pending_timers(&self) -> usize {
            self.inner.timers.borrow().len()
        }

        /// Poll every task until none makes progress. Time does not move.
        pub fn run(&self) {
            let flag = Arc::new(Flag(AtomicBool::new(false)));
            let waker = Waker::from(flag.clone());
            let mut cx = Context::from_waker(&waker);
            loop {
                // Tasks are taken out while they run, so a task may spawn another.
                let mut tasks = std::mem::take(&mut *self.inner.tasks.borrow_mut());
                tasks.extend(self.inner.spawned.borrow_mut().drain(..));
                flag.0.store(false, Ordering::SeqCst);
                tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
                *self.inner.tasks.borrow_mut() = tasks;
                // Another pass only if a poll woke a task or spawned one.
                if !flag.0.load(Ordering::SeqCst) && self.inner.spawned.borrow().is_empty() {
                    break;
                }
            }
        }

        /// Move the clock forward by `ms`, firing due timers in order (ties in
        /// scheduling order) and running tasks after each.
        pub fn advance(&self, ms: f64) {
            let target = self.inner.now.get() + ms;
            self.run();
            loop {
                let next = {
                    let mut timers = self.inner.timers.borrow_mut();
                    let pick = timers
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.due <= target)
                        .min_by(|(_, a), (_, b)| {
                            a.due
                                .partial_cmp(&b.due)
                                .unwrap_or(std::cmp::Ordering::Equal)
                                .then(a.id.cmp(&b.id))
                        })
                        .map(|(i, _)| i);
                    pick.map(|i| timers.remove(i))
                };
                let Some(timer) = next else { break };
                if timer.due > self.inner.now.get() {
                    self.inner.now.set(timer.due);
                }
                (timer.callback)();
                self.run();
            }
            self.inner.now.set(target);
        }

        /// Drive `future` to completion, advancing the clock through any timers
        /// it waits on. Panics if it is still pending with nothing left to wait
        /// for (a request no responder answers, a socket nobody drives).
        pub fn block_on<T: 'static>(&self, future: impl Future<Output = T> + 'static) -> T {
            let slot: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
            let out = slot.clone();
            self.spawn(Box::pin(async move {
                *out.borrow_mut() = Some(future.await);
            }));
            loop {
                self.run();
                if let Some(value) = slot.borrow_mut().take() {
                    return value;
                }
                let next_due = self
                    .inner
                    .timers
                    .borrow()
                    .iter()
                    .map(|t| t.due)
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    next_due.is_finite(),
                    "block_on: the future is pending and nothing is scheduled to wake it"
                );
                self.advance((next_due - self.inner.now.get()).max(0.0));
            }
        }
    }

    impl Platform for ManualPlatform {
        fn now_ms(&self) -> f64 {
            self.inner.now.get()
        }

        fn spawn(&self, future: LocalBoxFuture<'static, ()>) {
            self.inner.spawned.borrow_mut().push_back(future);
        }

        fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> TimerId {
            let id = self.inner.next_timer.get() + 1;
            self.inner.next_timer.set(id);
            let delay = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
            self.inner.timers.borrow_mut().push(Timer {
                id,
                due: self.inner.now.get() + delay,
                callback,
            });
            TimerId(id)
        }

        fn clear_timeout(&self, id: TimerId) {
            self.inner.timers.borrow_mut().retain(|t| t.id != id.0);
        }

        fn fetch(
            &self,
            request: HttpRequest,
        ) -> LocalBoxFuture<'static, Result<HttpResponse, NetError>> {
            self.inner.requests.borrow_mut().push(request.clone());
            if request.signal.as_ref().is_some_and(AbortSignal::aborted) {
                return Box::pin(async { Err(NetError::Aborted) });
            }
            let scripted = {
                let mut responder = self.inner.responder.borrow_mut();
                responder.as_mut().and_then(|respond| respond(&request))
            };
            if let Some(answer) = scripted {
                return Box::pin(async move { answer });
            }
            let (tx, rx) = oneshot();
            if let Some(signal) = &request.signal {
                let tx = tx.clone();
                signal.on_abort(move || tx.send(Err(NetError::Aborted)));
            }
            if let Some(ms) = request.timeout_ms {
                let tx = tx.clone();
                self.set_timeout(ms, Box::new(move || tx.send(Err(NetError::Timeout))));
            }
            self.inner.pending.borrow_mut().push((request, tx));
            Box::pin(rx)
        }

        fn open_socket(&self, url: &str, handlers: SocketHandlers) -> Rc<dyn Socket> {
            let socket = Rc::new(FakeSocket {
                url: url.to_string(),
                handlers: RefCell::new(handlers),
                open: Cell::new(false),
                closed: Cell::new(false),
                sent: RefCell::new(Vec::new()),
            });
            self.inner.sockets.borrow_mut().push(socket.clone());
            socket
        }

        fn warn(&self, message: &str) {
            self.inner.warnings.borrow_mut().push(message.to_string());
        }

        /// A fixed pseudo-random sequence (splitmix64), the same on every run.
        fn random(&self) -> f64 {
            let mut z = self
                .inner
                .random_state
                .get()
                .wrapping_add(0x9E37_79B9_7F4A_7C15);
            self.inner.random_state.set(z);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ManualPlatform;
    use super::*;

    #[test]
    fn sleep_resolves_only_when_the_clock_reaches_it() {
        let platform = ManualPlatform::new(1_000.0);
        let done = Rc::new(Cell::new(false));
        let flag = done.clone();
        let p = platform.clone();
        platform.spawn(Box::pin(async move {
            sleep(&p, 500.0).await;
            flag.set(true);
        }));
        platform.run();
        assert!(!done.get());
        platform.advance(499.0);
        assert!(!done.get());
        platform.advance(1.0);
        assert!(done.get());
        assert_eq!(platform.now_ms(), 1_500.0);
    }

    #[test]
    fn yield_now_suspends_once_and_the_executor_resumes_it() {
        let platform = ManualPlatform::default();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (l1, l2) = (log.clone(), log.clone());
        platform.spawn(Box::pin(async move {
            l1.borrow_mut().push("a before");
            yield_now().await;
            l1.borrow_mut().push("a after");
        }));
        platform.spawn(Box::pin(async move {
            l2.borrow_mut().push("b only");
        }));
        platform.run();
        assert_eq!(*log.borrow(), vec!["a before", "b only", "a after"]);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn js_await_continues_a_turn_later_even_on_a_ready_future() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let inner = log.clone();
        // An `async` function called without `await` returns at its first await.
        let mut call = Box::pin(async move {
            inner.borrow_mut().push("callee prefix");
            let v = js_await(std::future::ready(7)).await;
            inner.borrow_mut().push("callee continuation");
            v
        });
        let mut cx = Context::from_waker(Waker::noop());
        assert!(call.as_mut().poll(&mut cx).is_pending());
        log.borrow_mut().push("caller continues");
        assert_eq!(call.as_mut().poll(&mut cx), Poll::Ready(7));
        assert_eq!(
            *log.borrow(),
            vec!["callee prefix", "caller continues", "callee continuation"]
        );
        let platform = ManualPlatform::default();
        assert_eq!(platform.block_on(js_await(std::future::ready(5))), 5);
    }

    #[test]
    fn nested_js_awaits_cost_one_turn_per_level() {
        let platform = ManualPlatform::default();
        let turns = Rc::new(Cell::new(0u32));
        let t = turns.clone();
        platform.spawn(Box::pin(async move {
            js_await(js_await(js_await(std::future::ready(())))).await;
            t.set(t.get() + 1);
        }));
        // A counter task that ticks once per pass.
        let ticks = Rc::new(Cell::new(0u32));
        let tk = ticks.clone();
        let seen = Rc::new(Cell::new(0u32));
        let s = seen.clone();
        let t2 = turns.clone();
        platform.spawn(Box::pin(async move {
            while t2.get() == 0 {
                tk.set(tk.get() + 1);
                yield_now().await;
            }
            s.set(tk.get());
        }));
        platform.run();
        assert_eq!(turns.get(), 1);
        assert!(seen.get() >= 3, "three levels took at least three turns");
    }

    #[test]
    fn timers_fire_in_due_order_and_can_be_cleared() {
        let platform = ManualPlatform::default();
        let order = Rc::new(RefCell::new(Vec::new()));
        let push = |label: &'static str| {
            let order = order.clone();
            Box::new(move || order.borrow_mut().push(label)) as Box<dyn FnOnce()>
        };
        platform.set_timeout(30.0, push("c"));
        let cleared = platform.set_timeout(10.0, push("cleared"));
        platform.set_timeout(20.0, push("b"));
        platform.set_timeout(20.0, push("b2"));
        platform.set_timeout(5.0, push("a"));
        platform.clear_timeout(cleared);
        platform.advance(100.0);
        assert_eq!(*order.borrow(), vec!["a", "b", "b2", "c"]);
        assert_eq!(platform.pending_timers(), 0);
    }

    #[test]
    fn fetch_answers_from_the_script_and_block_on_drives_it() {
        let platform = ManualPlatform::default();
        platform.serve(vec![("/klines", "[1,2,3]".to_string())]);
        let p = platform.clone();
        let body = platform.block_on(async move {
            sleep(&p, 50.0).await;
            p.fetch(HttpRequest::get("https://api.example/klines?x=1"))
                .await
        });
        let response = body.unwrap();
        assert!(response.ok());
        assert_eq!(response.json().unwrap(), serde_json::json!([1, 2, 3]));
        assert_eq!(platform.requests().len(), 1);
    }

    #[test]
    fn an_unanswered_request_ends_on_abort_or_timeout() {
        let platform = ManualPlatform::default();
        let controller = AbortController::new();
        let aborted = Rc::new(RefCell::new(None));
        let timed_out = Rc::new(RefCell::new(None));
        let (p, a, t, signal) = (
            platform.clone(),
            aborted.clone(),
            timed_out.clone(),
            controller.signal(),
        );
        platform.spawn(Box::pin(async move {
            *a.borrow_mut() = Some(
                p.fetch(HttpRequest::get("https://x/slow").signal(signal))
                    .await,
            );
        }));
        let p = platform.clone();
        platform.spawn(Box::pin(async move {
            *t.borrow_mut() = Some(
                p.fetch(HttpRequest::get("https://x/slower").timeout(5_000.0))
                    .await,
            );
        }));
        platform.run();
        assert_eq!(platform.pending_requests(), 2);
        controller.abort();
        platform.run();
        assert_eq!(*aborted.borrow(), Some(Err(NetError::Aborted)));
        platform.advance(5_000.0);
        assert_eq!(*timed_out.borrow(), Some(Err(NetError::Timeout)));
        // A request made on an already-aborted signal fails at once.
        let late = platform.block_on(
            platform.fetch(HttpRequest::get("https://x/late").signal(controller.signal())),
        );
        assert_eq!(late, Err(NetError::Aborted));
    }

    #[test]
    fn abort_listeners_run_once_and_late_ones_at_once() {
        let controller = AbortController::new();
        let count = Rc::new(Cell::new(0));
        let c = count.clone();
        controller.signal().on_abort(move || c.set(c.get() + 1));
        controller.abort();
        controller.abort();
        assert_eq!(count.get(), 1);
        let c = count.clone();
        controller.signal().on_abort(move || c.set(c.get() + 10));
        assert_eq!(count.get(), 11);
    }

    #[test]
    fn a_fake_socket_is_driven_by_hand() {
        let platform = ManualPlatform::default();
        let log = Rc::new(RefCell::new(Vec::new()));
        let (l1, l2, l3) = (log.clone(), log.clone(), log.clone());
        let socket = platform.open_socket(
            "wss://stream.example/ws",
            SocketHandlers {
                on_open: Some(Box::new(move || l1.borrow_mut().push("open".to_string()))),
                on_message: Some(Box::new(move |text| l2.borrow_mut().push(text.to_string()))),
                on_error: None,
                on_close: Some(Box::new(move || l3.borrow_mut().push("close".to_string()))),
            },
        );
        socket.send("dropped: not open yet");
        let fake = platform.sockets()[0].clone();
        fake.emit_open();
        socket.send("subscribe");
        fake.emit_message("tick");
        socket.close();
        socket.close();
        assert_eq!(*log.borrow(), vec!["open", "tick", "close"]);
        assert_eq!(fake.sent(), vec!["subscribe"]);
        assert!(fake.is_closed());
        assert!(!socket.is_open());
    }
}
