//! PCM playback only. Load PulseAudio at runtime; never link it into the GUI.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::time::{Duration, Instant};

const FRAMES: usize = 960;
const SAMPLES: usize = FRAMES * 2;
const BYTES: usize = SAMPLES * size_of::<f32>();
const BUFFERS: usize = 3;
const SETUP: Duration = Duration::from_secs(3);
const OPERATION: Duration = Duration::from_millis(250);
const TIMING_EVERY: Duration = Duration::from_millis(250);
const STALE: Duration = Duration::from_secs(2);

type Object = *mut c_void;
type Success = Option<unsafe extern "C" fn(Object, i32, Object)>;
type Notify = Option<unsafe extern "C" fn(Object, Object)>;

#[repr(C)]
struct SampleSpec {
    format: i32,
    rate: u32,
    channels: u8,
}
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

macro_rules! api {
    ($($name:ident: $signature:ty),+ $(,)?) => {
        struct Api { library: Object, $($name: $signature),+ }
        impl Api {
            fn load() -> Result<Self, Unavailable> {
                // SAFETY: a fixed system SONAME, resolved now with local symbols.
                let library = unsafe { libc::dlopen(c"libpulse.so.0".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
                if library.is_null() { return Err(Unavailable); }
                let loaded = (|| {
                    $(
                        let name = CStr::from_bytes_with_nul(concat!(stringify!($name), "\0").as_bytes()).map_err(|_| Unavailable)?;
                        // SAFETY: the handle is live; each signature is the
                        // published PulseAudio C ABI for this fixed symbol.
                        let address = unsafe { libc::dlsym(library, name.as_ptr()) };
                        if address.is_null() { return Err(Unavailable); }
                        // SAFETY: supported Linux architectures use equally
                        // sized data and C function pointers; dlsym matched ABI.
                        let $name = unsafe { std::mem::transmute::<Object, $signature>(address) };
                    )+
                    Ok(Self { library, $($name),+ })
                })();
                if loaded.is_err() {
                    // SAFETY: no function pointer escaped the failed load.
                    unsafe { libc::dlclose(library); }
                }
                loaded
            }
        }
        impl Drop for Api {
            fn drop(&mut self) {
                // SAFETY: Output releases all objects before dropping Api.
                unsafe { libc::dlclose(self.library); }
            }
        }
    };
}

api! {
    pa_get_library_version: unsafe extern "C" fn() -> *const c_char,
    pa_mainloop_new: unsafe extern "C" fn() -> Object,
    pa_mainloop_free: unsafe extern "C" fn(Object),
    pa_mainloop_get_api: unsafe extern "C" fn(Object) -> Object,
    pa_mainloop_iterate: unsafe extern "C" fn(Object, i32, *mut i32) -> i32,
    pa_proplist_new: unsafe extern "C" fn() -> Object,
    pa_proplist_free: unsafe extern "C" fn(Object),
    pa_proplist_sets: unsafe extern "C" fn(Object, *const c_char, *const c_char) -> i32,
    pa_context_new_with_proplist: unsafe extern "C" fn(Object, *const c_char, Object) -> Object,
    pa_context_load_cookie_from_file: unsafe extern "C" fn(Object, *const c_char) -> i32,
    pa_context_connect: unsafe extern "C" fn(Object, *const c_char, i32, Object) -> i32,
    pa_context_get_state: unsafe extern "C" fn(Object) -> i32,
    pa_context_disconnect: unsafe extern "C" fn(Object),
    pa_context_unref: unsafe extern "C" fn(Object),
    pa_context_set_sink_input_mute: unsafe extern "C" fn(Object, u32, i32, Success, Object) -> Object,
    pa_stream_new: unsafe extern "C" fn(Object, *const c_char, *const SampleSpec, Object) -> Object,
    pa_stream_connect_playback: unsafe extern "C" fn(Object, *const c_char, *const BufferAttr, i32, Object, Object) -> i32,
    pa_stream_disconnect: unsafe extern "C" fn(Object) -> i32,
    pa_stream_unref: unsafe extern "C" fn(Object),
    pa_stream_get_state: unsafe extern "C" fn(Object) -> i32,
    pa_stream_get_index: unsafe extern "C" fn(Object) -> u32,
    pa_stream_writable_size: unsafe extern "C" fn(Object) -> usize,
    pa_stream_write: unsafe extern "C" fn(Object, *const c_void, usize, Option<unsafe extern "C" fn(Object)>, i64, i32) -> i32,
    pa_stream_cork: unsafe extern "C" fn(Object, i32, Success, Object) -> Object,
    pa_stream_flush: unsafe extern "C" fn(Object, Success, Object) -> Object,
    pa_stream_get_time: unsafe extern "C" fn(Object, *mut u64) -> i32,
    pa_stream_update_timing_info: unsafe extern "C" fn(Object, Success, Object) -> Object,
    pa_stream_set_latency_update_callback: unsafe extern "C" fn(Object, Notify, Object),
    pa_operation_get_state: unsafe extern "C" fn(Object) -> i32,
    pa_operation_cancel: unsafe extern "C" fn(Object),
    pa_operation_unref: unsafe extern "C" fn(Object),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

/// One regular mainloop, used by exactly one thread at a time. No input,
/// device enumeration, codec, filename or encoded-media API is exposed.
pub struct Output {
    api: Api,
    mainloop: Object,
    context: Object,
    stream: Object,
    timing: Box<Cell<Option<Instant>>>,
    requested: Cell<Instant>,
    playing: bool,
    reserved: bool,
    submitted: u64,
    retired: bool,
}

// SAFETY: Pulse's regular mainloop and its context may move between threads
// when calls remain serialized. Output is owned by one feeding thread and
// is not Sync; callbacks run only inside that thread's mainloop iteration.
unsafe impl Send for Output {}

unsafe extern "C" fn timing_updated(_stream: Object, user: Object) {
    // SAFETY: the boxed Cell stays at this address until stream unref; the
    // regular mainloop invokes this callback on Output's sole owning thread.
    unsafe { &*user.cast::<Cell<Option<Instant>>>() }.set(Some(Instant::now()));
}

unsafe extern "C" fn succeeded(_object: Object, success: i32, user: Object) {
    // SAFETY: perform keeps this Cell alive until completion or cancellation.
    unsafe { &*user.cast::<Cell<i32>>() }.set(if success != 0 { 1 } else { -1 });
}

impl Output {
    /// Establish the fixed local service connection before the broker
    /// confines itself. That broker must confine before accepting PCM.
    pub fn open(muted: bool) -> Result<Self, Unavailable> {
        let api = Api::load()?;
        // SAFETY: Pulse returns a static NUL-terminated version string.
        let version = unsafe { CStr::from_ptr((api.pa_get_library_version)()) };
        let major = version
            .to_str()
            .ok()
            .and_then(|v| v.split('.').next())
            .and_then(|v| v.parse::<u32>().ok());
        if major.is_none_or(|major| major < 15) {
            return Err(Unavailable);
        }
        // SAFETY: constructors return owned nullable pointers; all failure
        // paths below pass the initialized objects through Output's Drop.
        let mainloop = unsafe { (api.pa_mainloop_new)() };
        if mainloop.is_null() {
            return Err(Unavailable);
        }
        let mut output = Self {
            api,
            mainloop,
            context: ptr::null_mut(),
            stream: ptr::null_mut(),
            timing: Box::new(Cell::new(None)),
            requested: Cell::new(Instant::now()),
            playing: false,
            reserved: false,
            submitted: 0,
            retired: false,
        };
        // SAFETY: live mainloop; properties are copied by context creation
        // and released on every path. Fixed metadata avoids adding the user's
        // name, hostname or machine/session identifiers to local IPC payloads.
        unsafe {
            let properties = (output.api.pa_proplist_new)();
            if properties.is_null() {
                return Err(Unavailable);
            }
            let mut valid = true;
            for (key, value) in [
                // Since Pulse 15 this overrides shared-memory configuration;
                // keep FSIZE=0 instead of allocating a file-backed pool.
                (c"context.force.disable.shm", c"1"),
                (c"application.process.user", c"local"),
                (c"application.process.host", c"local"),
                (c"application.process.binary", c"kettle-media-worker"),
                (c"application.process.machine_id", c""),
                (c"application.process.session_id", c""),
            ] {
                valid &=
                    (output.api.pa_proplist_sets)(properties, key.as_ptr(), value.as_ptr()) == 0;
            }
            if valid {
                output.context = (output.api.pa_context_new_with_proplist)(
                    (output.api.pa_mainloop_get_api)(mainloop),
                    c"Kettle video".as_ptr(),
                    properties,
                );
            }
            (output.api.pa_proplist_free)(properties);
        }
        if output.context.is_null() {
            return Err(Unavailable);
        }
        // SAFETY: the fixed local Unix endpoint authenticates this process by
        // its kernel uid. A zero cookie avoids creating or changing a user's
        // cookie file; Pulse reads exactly 256 bytes from this fixed device.
        if unsafe {
            (output.api.pa_context_load_cookie_from_file)(output.context, c"/dev/zero".as_ptr())
        } < 0
        {
            return Err(Unavailable);
        }
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        let server = std::ffi::CString::new(format!("unix:/run/user/{uid}/pulse/native"))
            .map_err(|_| Unavailable)?;
        // SAFETY: live context, valid fixed local endpoint, NOAUTOSPAWN=1.
        if unsafe {
            (output.api.pa_context_connect)(output.context, server.as_ptr(), 1, ptr::null_mut())
        } < 0
        {
            return Err(Unavailable);
        }
        let deadline = Instant::now() + SETUP;
        loop {
            output.pump()?;
            // SAFETY: live context on its owning thread.
            match unsafe { (output.api.pa_context_get_state)(output.context) } {
                4 => break,
                5 | 6 => return Err(Unavailable),
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                _ => return Err(Unavailable),
            }
        }
        let spec = SampleSpec {
            format: 5,
            rate: 48_000,
            channels: 2,
        }; // FLOAT32LE
        let attr = BufferAttr {
            maxlength: (BUFFERS * BYTES) as u32,
            tlength: (BUFFERS * BYTES) as u32,
            prebuf: 0,
            minreq: BYTES as u32,
            fragsize: u32::MAX,
        };
        // SAFETY: live context, specified stereo format; no custom channel map.
        output.stream = unsafe {
            (output.api.pa_stream_new)(output.context, c"Video".as_ptr(), &spec, ptr::null_mut())
        };
        if output.stream.is_null() {
            return Err(Unavailable);
        }
        // SAFETY: the boxed timing cell outlives the stream and all callbacks.
        unsafe {
            (output.api.pa_stream_set_latency_update_callback)(
                output.stream,
                Some(timing_updated),
                (&*output.timing as *const Cell<Option<Instant>>)
                    .cast_mut()
                    .cast(),
            );
            // START_CORKED | INTERPOLATE_TIMING | AUTO_TIMING_UPDATE.
            if (output.api.pa_stream_connect_playback)(
                output.stream,
                ptr::null(),
                &attr,
                1 | 2 | 8,
                ptr::null_mut(),
                ptr::null_mut(),
            ) < 0
            {
                return Err(Unavailable);
            }
        }
        loop {
            output.pump()?;
            // SAFETY: live stream on its owning thread.
            match unsafe { (output.api.pa_stream_get_state)(output.stream) } {
                2 => break,
                3 | 4 => return Err(Unavailable),
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(1)),
                _ => return Err(Unavailable),
            }
        }
        output.set_muted(muted)?;
        Ok(output)
    }

    fn pump(&self) -> Result<(), Unavailable> {
        for _ in 0..8 {
            let mut status = 0;
            // SAFETY: regular, nonblocking mainloop, used only by its owner.
            match unsafe { (self.api.pa_mainloop_iterate)(self.mainloop, 0, &mut status) } {
                n if n < 0 => return Err(Unavailable),
                0 => break,
                _ => {}
            }
        }
        Ok(())
    }

    fn perform(&self, start: impl FnOnce(Success, Object) -> Object) -> Result<(), Unavailable> {
        let success = Cell::new(0);
        let operation = start(
            Some(succeeded),
            (&success as *const Cell<i32>).cast_mut().cast(),
        );
        if operation.is_null() {
            return Err(Unavailable);
        }
        let deadline = Instant::now() + OPERATION;
        let result = loop {
            if self.pump().is_err() {
                break Err(Unavailable);
            }
            // SAFETY: operation remains referenced until after this loop.
            if unsafe { (self.api.pa_operation_get_state)(operation) } != 0 {
                break if success.get() == 1 {
                    Ok(())
                } else {
                    Err(Unavailable)
                };
            }
            if Instant::now() >= deadline {
                break Err(Unavailable);
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        // SAFETY: cancellation prevents future success callbacks before the
        // stack Cell expires. Unref releases exactly our operation reference.
        unsafe {
            (self.api.pa_operation_cancel)(operation);
            (self.api.pa_operation_unref)(operation);
        }
        result
    }

    pub fn take_free(&mut self) -> Option<usize> {
        self.pump().ok()?;
        if self.retired || self.reserved {
            return None;
        }
        let played = self.position().unwrap_or(0.0) as u64;
        if self.submitted.saturating_sub(played) + FRAMES as u64 > (BUFFERS * FRAMES) as u64 {
            return None;
        }
        // SAFETY: live stream. usize::MAX denotes an error.
        let available = unsafe { (self.api.pa_stream_writable_size)(self.stream) };
        if available == usize::MAX || available < BYTES {
            return None;
        }
        self.reserved = true;
        Some(0)
    }

    pub fn enqueue(&mut self, index: usize, samples: &[f32]) -> Result<(), Unavailable> {
        if self.retired
            || index != 0
            || !self.reserved
            || samples.len() != SAMPLES
            || !samples.iter().all(|n| n.is_finite())
        {
            return Err(Unavailable);
        }
        self.reserved = false;
        // SAFETY: the live stream copies exactly the samples' byte slice;
        // no free callback means it retains no pointer into Rust memory.
        if unsafe {
            (self.api.pa_stream_write)(self.stream, samples.as_ptr().cast(), BYTES, None, 0, 0)
        } < 0
        {
            return Err(Unavailable);
        }
        self.submitted += FRAMES as u64;
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), Unavailable> {
        if self.retired {
            return Err(Unavailable);
        }
        if !self.playing {
            // SAFETY: live stream, callback and userdata remain valid in perform.
            self.perform(|cb, user| unsafe {
                (self.api.pa_stream_cork)(self.stream, 0, cb, user)
            })?;
            self.playing = true;
        }
        Ok(())
    }

    pub fn pause(&mut self) -> Result<(), Unavailable> {
        if self.playing {
            // SAFETY: live stream, callback and userdata remain valid in perform.
            self.perform(|cb, user| unsafe {
                (self.api.pa_stream_cork)(self.stream, 1, cb, user)
            })?;
            self.playing = false;
        }
        Ok(())
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), Unavailable> {
        // SAFETY: live ready stream and context, on their sole owning thread.
        let index = unsafe { (self.api.pa_stream_get_index)(self.stream) };
        if index == u32::MAX {
            return Err(Unavailable);
        }
        // SAFETY: live context; perform bounds and owns the callback lifetime.
        self.perform(|cb, user| unsafe {
            (self.api.pa_context_set_sink_input_mute)(
                self.context,
                index,
                i32::from(muted),
                cb,
                user,
            )
        })
    }

    /// A seek retires the broker generation. Flush before retiring so no
    /// samples from that generation remain queued while its successor opens.
    pub fn flush(&mut self) -> Result<(), Unavailable> {
        self.pause()?;
        // SAFETY: live stream, callback and userdata remain valid in perform.
        self.perform(|cb, user| unsafe { (self.api.pa_stream_flush)(self.stream, cb, user) })?;
        self.retired = true;
        self.reserved = false;
        self.timing.set(None);
        Ok(())
    }

    fn position(&self) -> Option<f64> {
        self.pump().ok()?;
        if self.requested.get().elapsed() >= TIMING_EVERY {
            // SAFETY: live stream; no callback or user data is retained. Pulse
            // keeps its own operation reference until the request completes.
            let operation = unsafe {
                (self.api.pa_stream_update_timing_info)(self.stream, None, ptr::null_mut())
            };
            if operation.is_null() {
                return None;
            }
            // SAFETY: release our returned reference, not Pulse's internal one.
            unsafe {
                (self.api.pa_operation_unref)(operation);
            }
            self.requested.set(Instant::now());
        }
        if self.timing.get()?.elapsed() > STALE {
            return None;
        }
        let mut us = 0;
        // SAFETY: live stream and a valid u64 output slot. This already is
        // hardware presentation time; never subtract stream latency again.
        if unsafe { (self.api.pa_stream_get_time)(self.stream, &mut us) } < 0 {
            return None;
        }
        Some((us as f64 * 48_000.0 / 1_000_000.0).min(self.submitted as f64))
    }

    pub fn played(&self) -> Option<(f64, Instant)> {
        self.playing
            .then(|| self.position().map(|frames| (frames, Instant::now())))
            .flatten()
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        // SAFETY: release in reverse construction order. The boxed callback
        // state and library remain live throughout all disconnect/unref calls.
        unsafe {
            if !self.stream.is_null() {
                (self.api.pa_stream_set_latency_update_callback)(
                    self.stream,
                    None,
                    ptr::null_mut(),
                );
                (self.api.pa_stream_disconnect)(self.stream);
                (self.api.pa_stream_unref)(self.stream);
            }
            if !self.context.is_null() {
                (self.api.pa_context_disconnect)(self.context);
                (self.api.pa_context_unref)(self.context);
            }
            (self.api.pa_mainloop_free)(self.mainloop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a running local PulseAudio-compatible service"]
    fn real_service_corks_mutes_and_retires_one_generation() {
        let mut output = Output::open(false).expect("local PulseAudio playback");
        let tone: Vec<f32> = (0..SAMPLES)
            .map(|n| ((n / 2) as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.1)
            .collect();
        for _ in 0..BUFFERS {
            let index = output.take_free().expect("initial credit");
            output.enqueue(index, &tone).unwrap();
        }
        assert!(output.take_free().is_none(), "unbounded prebuffer");
        assert!(
            output.played().is_none(),
            "opened stream played automatically"
        );
        output.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if output
                .played()
                .is_some_and(|(frames, _)| frames >= FRAMES as f64)
            {
                break;
            }
            assert!(Instant::now() < deadline, "no presentation clock");
            std::thread::sleep(Duration::from_millis(5));
        }
        while let Some(index) = output.take_free() {
            output.enqueue(index, &tone).unwrap();
        }
        output.pause().unwrap();
        let paused = output.position().expect("paused position");
        std::thread::sleep(Duration::from_millis(120));
        let after = output.position().expect("retained paused position");
        assert!(
            (after - paused).abs() < 240.0,
            "corked clock advanced: {paused} -> {after}"
        );
        output.set_muted(true).unwrap();
        while let Some(index) = output.take_free() {
            output.enqueue(index, &tone).unwrap();
        }
        output.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if output
                .played()
                .is_some_and(|(frames, _)| frames > after + 240.0)
            {
                break;
            }
            assert!(Instant::now() < deadline, "mute stopped the clock");
            std::thread::sleep(Duration::from_millis(5));
        }
        output.set_muted(false).unwrap();
        output.flush().unwrap();
        assert!(output.take_free().is_none());
        assert_eq!(
            output.start(),
            Err(Unavailable),
            "retired generation restarted"
        );
    }
}
