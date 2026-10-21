use super::state::Runtime;
use rpi_plugin_sdk::{
    self as sdk,
    callbacks::{free_string, OwnedString},
    PluginApi, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
    ToolWakeCb,
};
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex, Weak,
    },
    thread::JoinHandle,
    time::Duration,
};

pub(super) struct Definition {
    runtime: Weak<Runtime>,
    name: String,
}
impl Definition {
    pub fn new(runtime: &Arc<Runtime>, value: &Value) -> Result<Self, String> {
        if !value["parameters"].is_object() || !value["description"].is_string() {
            return Err("Invalid Cursor tool schema".into());
        }
        if value["exposure"] != "direct" {
            return Err("Cursor native tools require direct exposure".into());
        }
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            name: value["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or("Invalid Cursor tool name")?
                .into(),
        })
    }
    pub fn register(&self, api: &PluginApi, value: &Value) -> i32 {
        let schema = StableToolSchema {
            name: StbString::from_string(self.name.clone()),
            description: StbString::from_string(value["description"].as_str().unwrap().into()),
            parameters: StbString::from_string(value["parameters"].to_string()),
        };
        let status = (api.register_owned_tool_with_wake.unwrap())(
            &schema,
            execute,
            poll,
            cancel,
            destroy,
            free_string,
            set_waker,
            (self as *const Self).cast_mut().cast(),
        );
        if status != 0 {
            return status;
        }
        let mut metadata = serde_json::Map::new();
        for key in [
            "label",
            "executionMode",
            "promptSnippet",
            "promptGuidelines",
        ] {
            if let Some(value) = value.get(key) {
                metadata.insert(key.into(), value.clone());
            }
        }
        (api.declare.unwrap())(StbStringRef::from_str(
            &json!({"toolMetadata": {&self.name: metadata}}).to_string(),
        ))
    }
}
#[derive(Default)]
struct Progress {
    partial: Option<Value>,
    result: Option<Result<Value, String>>,
    waker: Option<(ToolWakeCb, usize)>,
}
struct Work {
    progress: Mutex<Progress>,
    available: Condvar,
    cancelled: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}
impl Work {
    fn wake(waker: Option<(ToolWakeCb, usize)>) {
        if let Some((callback, data)) = waker {
            callback(data as *mut c_void);
        }
    }
    fn emit(
        &self,
        partial: Value,
        signal: &tokio_util::sync::CancellationToken,
    ) -> Result<(), String> {
        let mut progress = self.progress.lock().unwrap();
        while progress.partial.is_some()
            && !self.cancelled.load(Ordering::Acquire)
            && !signal.is_cancelled()
        {
            progress = self
                .available
                .wait_timeout(progress, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        if self.cancelled.load(Ordering::Acquire) || signal.is_cancelled() {
            return Err("Cursor tool cancelled".into());
        }
        progress.partial = Some(partial);
        let waker = progress.waker;
        drop(progress);
        Self::wake(waker);
        Ok(())
    }
    fn finish(&self, result: Result<Value, String>) {
        let mut progress = self.progress.lock().unwrap();
        progress.result = Some(result);
        let waker = progress.waker;
        drop(progress);
        Self::wake(waker);
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.available.notify_all();
    }
}
static HANDLES: sdk::handles::ToolHandles<Arc<Work>> = sdk::handles::ToolHandles::new();
fn work(handle: StepHandle) -> Option<Arc<Work>> {
    HANDLES.get(handle).map(|entry| entry.lock().clone())
}

extern "C" fn execute(
    id: StbStringRef,
    params: StbString,
    free: Option<sdk::FreeStringFn>,
    context: *mut c_void,
) -> StepHandle {
    sdk::guard_or(std::ptr::null_mut(), || {
        let params = unsafe { OwnedString::from_raw(params, free) };
        let work = Arc::new(Work {
            progress: Mutex::new(Progress::default()),
            available: Condvar::new(),
            cancelled: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        let handle = HANDLES.insert(work.clone());
        let prepared = (|| {
            let definition = unsafe { context.cast::<Definition>().as_ref() }
                .ok_or("Cursor tool definition expired")?;
            let runtime = definition
                .runtime
                .upgrade()
                .ok_or("Cursor tool owner expired")?;
            let client = runtime.client.scoped()?;
            let mut args: Value = serde_json::from_str(&params.to_string_lossy())
                .map_err(|_| "Invalid Cursor tool arguments")?;
            args.as_object_mut()
                .ok_or("Cursor tool arguments must be an object")?
                .remove(sdk::HOST_CONTEXT_KEY);
            Ok::<_, String>((
                runtime,
                client,
                json!({"name":definition.name,"id":unsafe { id.as_str() },"args":args}),
            ))
        })();
        match prepared {
            Err(error) => work.finish(Err(error)),
            Ok((runtime, client, input)) => {
                let job = work.clone();
                match std::thread::Builder::new()
                    .name("cursor-tool".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            runtime.invoke(
                                client,
                                "extension.tool",
                                input,
                                &|| job.cancelled.load(Ordering::Acquire),
                                |partial, signal| job.emit(partial, signal),
                            )
                        }))
                        .unwrap_or_else(|_| Err("Cursor tool worker stopped".into()));
                        job.finish(result);
                    }) {
                    Ok(thread) => *work.thread.lock().unwrap() = Some(thread),
                    Err(_) => work.finish(Err("Could not start Cursor tool callback".into())),
                }
            }
        }
        handle
    })
}
extern "C" fn poll(
    handle: StepHandle,
    partial: Option<ToolPartialCb>,
    data: *mut c_void,
) -> StepResult {
    sdk::guard_or(StepResult::err(StbString::empty()), || {
        let Some(work) = work(handle) else {
            return StepResult::err(StbString::from_string("Cursor tool handle expired".into()));
        };
        let (update, result, wake) = {
            let mut progress = work.progress.lock().unwrap();
            let update = progress.partial.take();
            if partial.is_none() && update.is_some() {
                let wake = progress.result.as_ref().and(progress.waker);
                (update, None, wake)
            } else {
                (update, progress.result.take(), None)
            }
        };
        Work::wake(wake);
        work.available.notify_one();
        if let (Some(callback), Some(update)) = (partial, update.as_ref()) {
            callback(StbString::from_string(update.to_string()), data);
        }
        match result {
            Some(Ok(value)) => StepResult::done(StbString::from_string(value.to_string())),
            Some(Err(error)) => StepResult::err(StbString::from_string(error)),
            None => StepResult::pending(if partial.is_none() {
                update
                    .map(|value| StbString::from_string(value.to_string()))
                    .unwrap_or_else(StbString::empty)
            } else {
                StbString::empty()
            }),
        }
    })
}
extern "C" fn cancel(handle: StepHandle) {
    sdk::guard_or((), || {
        if let Some(work) = work(handle) {
            work.cancel();
        }
    });
}
extern "C" fn destroy(handle: StepHandle) {
    sdk::guard_or((), || {
        if let Some(entry) = HANDLES.remove(handle) {
            let work = entry.lock().clone();
            work.cancel();
            let thread = work.thread.lock().unwrap().take();
            if let Some(thread) = thread {
                let _ = thread.join();
            }
        }
    });
}
extern "C" fn set_waker(handle: StepHandle, callback: Option<ToolWakeCb>, data: *mut c_void) {
    sdk::guard_or((), || {
        if let Some(work) = work(handle) {
            let mut progress = work.progress.lock().unwrap();
            progress.waker = callback.map(|callback| (callback, data as usize));
            let wake = if progress.partial.is_some() || progress.result.is_some() {
                progress.waker
            } else {
                None
            };
            drop(progress);
            Work::wake(wake);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job() -> Arc<Work> {
        Arc::new(Work {
            progress: Mutex::new(Progress::default()),
            available: Condvar::new(),
            cancelled: AtomicBool::new(false),
            thread: Mutex::new(None),
        })
    }
    extern "C" fn wake(data: *mut c_void) {
        unsafe { &*data.cast::<std::sync::atomic::AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }
    extern "C" fn partial(value: StbString, data: *mut c_void) {
        let value = unsafe { OwnedString::from_raw(value, Some(free_string)) };
        assert_eq!(
            serde_json::from_str::<Value>(&value.to_string_lossy()).unwrap(),
            json!({"partial":1})
        );
        wake(data);
    }
    #[test]
    fn wakes_for_already_ready_work_and_emits_partial_before_terminal() {
        let work = job();
        let handle = HANDLES.insert(work.clone());
        work.emit(json!({"partial":1}), &Default::default())
            .unwrap();
        work.finish(Ok(json!({"done":true})));
        let hits = std::sync::atomic::AtomicUsize::new(0);
        let data = (&hits as *const std::sync::atomic::AtomicUsize)
            .cast_mut()
            .cast();
        set_waker(handle, Some(wake), data);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let result = poll(handle, Some(partial), data);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(result.tag, sdk::StepResultTag::Done);
        unsafe {
            sdk::callbacks::free_step_result(result);
        }
        set_waker(handle, None, std::ptr::null_mut());
        destroy(handle);
        destroy(handle);
        assert!(HANDLES.get(handle).is_none());
    }
    #[test]
    fn destroy_cancels_bounded_progress_and_joins_the_callback_thread() {
        let work = job();
        let handle = HANDLES.insert(work.clone());
        work.emit(json!({"partial":1}), &Default::default())
            .unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let completed = Arc::new(AtomicBool::new(false));
        *work.thread.lock().unwrap() = Some(std::thread::spawn({
            let work = work.clone();
            let completed = completed.clone();
            move || {
                started.send(()).unwrap();
                assert!(work
                    .emit(json!({"partial":2}), &Default::default())
                    .is_err());
                completed.store(true, Ordering::Release);
            }
        }));
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(!completed.load(Ordering::Acquire));
        destroy(handle);
        assert!(completed.load(Ordering::Acquire));
        assert!(HANDLES.get(handle).is_none());
    }
    #[test]
    fn scope_shutdown_releases_a_stopped_progress_consumer() {
        let work = job();
        let handle = HANDLES.insert(work.clone());
        let signal = tokio_util::sync::CancellationToken::new();
        work.emit(json!({"partial":1}), &signal).unwrap();
        let (done, completed) = std::sync::mpsc::channel();
        *work.thread.lock().unwrap() = Some(std::thread::spawn({
            let work = work.clone();
            let signal = signal.clone();
            move || {
                done.send(work.emit(json!({"partial":2}), &signal)).unwrap();
            }
        }));
        signal.cancel();
        assert!(completed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .is_err());
        destroy(handle);
    }
    #[test]
    fn optional_partial_callback_still_preserves_progress_before_completion() {
        let work = job();
        let handle = HANDLES.insert(work.clone());
        work.emit(json!({"partial":1}), &Default::default())
            .unwrap();
        work.finish(Ok(json!({"done":true})));
        for expected in [sdk::StepResultTag::Pending, sdk::StepResultTag::Done] {
            let result = poll(handle, None, std::ptr::null_mut());
            assert_eq!(result.tag, expected);
            unsafe {
                sdk::callbacks::free_step_result(result);
            }
        }
        destroy(handle);
    }
}
