//! [`vela_core::runtime::Platform`] on the browser's event loop: `setTimeout`,
//! `fetch`, `WebSocket`, `Date.now` and promise scheduling.
//!
//! The JavaScript side is the snippet below. It uses only globals that exist
//! in a window, a worker and Node alike, so the same build runs in all three.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Function, Promise};
use vela_core::runtime::{
    HttpRequest, HttpResponse, LocalBoxFuture, NetError, Platform, Socket, SocketHandlers, TimerId,
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};

#[wasm_bindgen(inline_js = r#"
const timers = new Map();

export function vela_set_timeout(id, ms, dispatch) {
    timers.set(
        id,
        setTimeout(() => {
            timers.delete(id);
            dispatch(id);
        }, ms),
    );
}

export function vela_clear_timeout(id) {
    const handle = timers.get(id);
    if (handle !== undefined) {
        clearTimeout(handle);
        timers.delete(id);
    }
}

// Returns [promise, abort]. The promise resolves to [status, flatHeaders, body] and
// rejects with 'aborted', 'timeout' or the transport's error text.
export function vela_fetch(url, method, flatHeaders, body, timeoutMs) {
    const controller = new AbortController();
    let timedOut = false;
    let timer;
    if (timeoutMs != null) {
        timer = setTimeout(() => {
            timedOut = true;
            controller.abort();
        }, timeoutMs);
    }
    const init = { method, signal: controller.signal };
    if (flatHeaders.length > 0) {
        init.headers = {};
        for (let i = 0; i + 1 < flatHeaders.length; i += 2) init.headers[flatHeaders[i]] = flatHeaders[i + 1];
    }
    if (body != null) init.body = body;
    const promise = fetch(url, init)
        .then(async (response) => {
            const text = await response.text();
            const headers = [];
            response.headers.forEach((value, name) => headers.push(name.toLowerCase(), value));
            return [response.status, headers, text];
        })
        .catch((error) => {
            throw timedOut ? 'timeout' : controller.signal.aborted ? 'aborted' : String(error?.message ?? error);
        })
        .finally(() => clearTimeout(timer));
    return [promise, () => controller.abort()];
}

export function vela_socket_open(url, onOpen, onMessage, onError, onClose) {
    const socket = new WebSocket(url);
    socket.onopen = () => onOpen();
    socket.onmessage = (event) => {
        if (typeof event.data === 'string') onMessage(event.data);
    };
    socket.onerror = () => onError();
    socket.onclose = () => onClose();
    return socket;
}

export function vela_socket_send(socket, text) {
    if (socket.readyState === 1) socket.send(text);
}

export function vela_socket_close(socket) {
    try {
        socket.close();
    } catch {
        // Closing a socket that never opened may throw in some engines.
    }
}

export function vela_socket_is_open(socket) {
    return socket.readyState === 1;
}

// The Rust side is going away: stop calling into it, then close.
export function vela_socket_detach(socket) {
    socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null;
    vela_socket_close(socket);
}

export function vela_warn(message) {
    console.warn(message);
}
"#)]
extern "C" {
    fn vela_set_timeout(id: f64, ms: f64, dispatch: &Function);
    fn vela_clear_timeout(id: f64);
    fn vela_fetch(
        url: &str,
        method: &str,
        flat_headers: &Array,
        body: Option<String>,
        timeout_ms: Option<f64>,
    ) -> Array;
    fn vela_socket_open(
        url: &str,
        on_open: &Function,
        on_message: &Function,
        on_error: &Function,
        on_close: &Function,
    ) -> JsValue;
    fn vela_socket_send(socket: &JsValue, text: &str);
    fn vela_socket_close(socket: &JsValue);
    fn vela_socket_is_open(socket: &JsValue) -> bool;
    fn vela_socket_detach(socket: &JsValue);
    fn vela_warn(message: &str);
}

type TimerMap = Rc<RefCell<HashMap<u64, Box<dyn FnOnce()>>>>;

thread_local! {
    static NEXT_TIMER: Rc<Cell<u64>> = Rc::new(Cell::new(0));
}

/// The browser as core's [`Platform`]. Clone it freely: clones share timers.
#[derive(Clone)]
pub struct BrowserPlatform {
    timers: TimerMap,
    next_timer: Rc<Cell<u64>>,
    /// The one JavaScript-callable function every timer fires through. Routing
    /// all timers through it keeps a cleared timer from leaking a closure.
    dispatch: Rc<Closure<dyn FnMut(f64)>>,
}

impl Drop for BrowserPlatform {
    /// The last clone going away takes its pending timers with it: a timer left in the
    /// browser would fire through the dispatch closure this drop frees.
    fn drop(&mut self) {
        if Rc::strong_count(&self.dispatch) > 1 {
            return;
        }
        let ids: Vec<u64> = self.timers.borrow().keys().copied().collect();
        for id in ids {
            vela_clear_timeout(id as f64);
        }
    }
}

impl Default for BrowserPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserPlatform {
    pub fn new() -> Self {
        let timers: TimerMap = Rc::new(RefCell::new(HashMap::new()));
        let fired = timers.clone();
        let dispatch = Closure::<dyn FnMut(f64)>::new(move |id: f64| {
            // Take the callback out before running it: it may set or clear timers.
            let callback = fired.borrow_mut().remove(&(id as u64));
            if let Some(callback) = callback {
                callback();
            }
        });
        Self {
            timers,
            // One counter for every platform on the page: the JavaScript timer table is
            // shared, and two charts counting from 1 would overwrite each other's timers.
            next_timer: NEXT_TIMER.with(Rc::clone),
            dispatch: Rc::new(dispatch),
        }
    }

    /// This platform as the trait object core code takes.
    pub fn handle(&self) -> Rc<dyn Platform> {
        Rc::new(self.clone())
    }
}

fn parse_response(value: JsValue) -> Result<HttpResponse, NetError> {
    let malformed = || NetError::Failed("malformed fetch result".to_string());
    let fields: Array = value.dyn_into().map_err(|_| malformed())?;
    let status = fields.get(0).as_f64().ok_or_else(malformed)? as u16;
    let flat: Array = fields.get(1).dyn_into().map_err(|_| malformed())?;
    let mut headers = Vec::with_capacity(flat.length() as usize / 2);
    let mut i = 0;
    while i + 1 < flat.length() {
        if let (Some(name), Some(value)) = (flat.get(i).as_string(), flat.get(i + 1).as_string()) {
            headers.push((name, value));
        }
        i += 2;
    }
    Ok(HttpResponse {
        status,
        headers,
        body: fields.get(2).as_string().unwrap_or_default(),
    })
}

fn parse_failure(reason: JsValue) -> NetError {
    match reason.as_string().as_deref() {
        Some("aborted") => NetError::Aborted,
        Some("timeout") => NetError::Timeout,
        Some(message) => NetError::Failed(message.to_string()),
        None => NetError::Failed("fetch failed".to_string()),
    }
}

impl Platform for BrowserPlatform {
    fn now_ms(&self) -> f64 {
        js_sys::Date::now()
    }

    fn spawn(&self, future: LocalBoxFuture<'static, ()>) {
        spawn_local(future);
    }

    fn set_timeout(&self, ms: f64, callback: Box<dyn FnOnce()>) -> TimerId {
        let id = self.next_timer.get() + 1;
        self.next_timer.set(id);
        self.timers.borrow_mut().insert(id, callback);
        let delay = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
        vela_set_timeout(
            id as f64,
            delay,
            self.dispatch.as_ref().as_ref().unchecked_ref(),
        );
        TimerId(id)
    }

    fn clear_timeout(&self, id: TimerId) {
        if self.timers.borrow_mut().remove(&id.0).is_some() {
            vela_clear_timeout(id.0 as f64);
        }
    }

    fn fetch(
        &self,
        request: HttpRequest,
    ) -> LocalBoxFuture<'static, Result<HttpResponse, NetError>> {
        if request.signal.as_ref().is_some_and(|s| s.aborted()) {
            return Box::pin(async { Err(NetError::Aborted) });
        }
        let headers = Array::new();
        for (name, value) in &request.headers {
            headers.push(&JsValue::from_str(name));
            headers.push(&JsValue::from_str(value));
        }
        let started = vela_fetch(
            &request.url,
            &request.method,
            &headers,
            request.body.clone(),
            request.timeout_ms,
        );
        let promise: Promise = started.get(0).unchecked_into();
        if let Some(signal) = &request.signal {
            let abort: Function = started.get(1).unchecked_into();
            signal.on_abort(move || {
                let _ = abort.call0(&JsValue::NULL);
            });
        }
        Box::pin(async move {
            match JsFuture::from(promise).await {
                Ok(value) => parse_response(value),
                Err(reason) => Err(parse_failure(reason)),
            }
        })
    }

    fn open_socket(&self, url: &str, handlers: SocketHandlers) -> Rc<dyn Socket> {
        let SocketHandlers {
            on_open,
            on_message,
            on_error,
            on_close,
        } = handlers;
        let wrap = |handler: Option<Box<dyn FnMut()>>| {
            let mut handler = handler;
            Closure::<dyn FnMut()>::new(move || {
                if let Some(handler) = handler.as_mut() {
                    handler();
                }
            })
        };
        let open = wrap(on_open);
        let error = wrap(on_error);
        let close = wrap(on_close);
        let mut on_message = on_message;
        let message = Closure::<dyn FnMut(String)>::new(move |text: String| {
            if let Some(handler) = on_message.as_mut() {
                handler(&text);
            }
        });
        let socket = vela_socket_open(
            url,
            open.as_ref().unchecked_ref(),
            message.as_ref().unchecked_ref(),
            error.as_ref().unchecked_ref(),
            close.as_ref().unchecked_ref(),
        );
        Rc::new(BrowserSocket {
            socket,
            _open: open,
            _message: message,
            _error: error,
            _close: close,
        })
    }

    fn warn(&self, message: &str) {
        vela_warn(message);
    }

    fn random(&self) -> f64 {
        js_sys::Math::random()
    }
}

/// A browser `WebSocket` and the closures its events call.
struct BrowserSocket {
    socket: JsValue,
    _open: Closure<dyn FnMut()>,
    _message: Closure<dyn FnMut(String)>,
    _error: Closure<dyn FnMut()>,
    _close: Closure<dyn FnMut()>,
}

impl Socket for BrowserSocket {
    fn send(&self, text: &str) {
        vela_socket_send(&self.socket, text);
    }

    fn close(&self) {
        vela_socket_close(&self.socket);
    }

    fn is_open(&self) -> bool {
        vela_socket_is_open(&self.socket)
    }
}

impl Drop for BrowserSocket {
    /// The closures die with this value, so the socket must stop calling them.
    fn drop(&mut self) {
        vela_socket_detach(&self.socket);
    }
}
