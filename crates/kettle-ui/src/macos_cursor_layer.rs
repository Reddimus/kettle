//! The Core Animation layer that blinks the cursor on macOS.
//!
//! A `CAMetalLayer` sits directly above the window's wgpu Metal layer. A
//! `Renderer::attach_cursor_layer` gives the renderer a surface on it. After
//! a successful off-phase frame, `Renderer::present_cursor_patch` renders the
//! patch there, then [`CursorLayer::show`] moves it over
//! the cursor and starts a discrete opacity animation, so the window server
//! blinks the cursor while Kettle presents nothing and does not wake.
//! `cursor_blink` holds the timeline; `App::redraw` owns entry and exit.
//!
//! Cross-platform on purpose, with the `cfg` inside `imp`, like `macos_dock`:
//! off macOS a `CursorLayer` can never exist, so its call sites compile on
//! every target and never run.
//!
//! Every mutation runs in its own `CATransaction` with implicit actions off,
//! then flushes. `App::redraw` runs from winit's before-waiting run-loop
//! observer, which fires after Core Animation's own commit observer, so a
//! change left to an implicit transaction would wait for the next wake. The
//! layer has no delegate and the animation no completion block: nothing here
//! calls back into the app.

use std::time::Instant;

use crate::cursor_blink::LayerBlinkPlan;

/// Whether this platform can blink the cursor in a Core Animation layer.
pub(crate) const SUPPORTED: bool = cfg!(target_os = "macos");

/// One window's cursor layer. Main thread only.
pub(crate) struct CursorLayer {
    imp: imp::Layer,
}

impl CursorLayer {
    /// Add a hidden layer directly above the window's Metal layer.
    pub(crate) fn install(window: &winit::window::Window) -> Result<Self, &'static str> {
        imp::Layer::install(window).map(|imp| Self { imp })
    }

    /// Give the renderer a surface on this layer for the cursor patch.
    pub(crate) fn attach(
        &self,
        renderer: &mut kettle_render::Renderer,
    ) -> Result<(), &'static str> {
        self.imp.attach(renderer)
    }

    /// Put the layer over `rect_px` (device pixels, top-left origin) and start
    /// the blink `plan` describes.
    pub(crate) fn show(
        &self,
        rect_px: [u32; 4],
        scale: f64,
        plan: &LayerBlinkPlan,
        now: Instant,
    ) -> Result<(), &'static str> {
        if !plan.can_start(now) {
            return Err("the hand-off frame arrived too late");
        }
        self.imp.show(rect_px, scale, plan, now)
    }

    /// Stop the blink and hide the layer.
    pub(crate) fn hide(&self) {
        self.imp.hide();
    }

    /// Open the transaction an exit frame presents in: the Metal layer
    /// presents with it, so the frame and [`hide`](Self::hide) reach the
    /// screen together. Call before the frame acquires its drawable.
    pub(crate) fn begin_exit_frame(&self) {
        self.imp.begin_exit_frame();
    }

    /// Commit the exit frame's transaction and restore ordinary presents.
    pub(crate) fn end_exit_frame(&self) {
        self.imp.end_exit_frame();
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::time::Instant;

    use objc2_06::rc::Retained;
    use objc2_06::runtime::AnyObject;
    use objc2_core_foundation_03::{CGPoint, CGRect, CGSize};
    use objc2_foundation_03::{NSArray, NSNumber, NSString, ns_string};
    use objc2_quartz_core_03::{
        CAAutoresizingMask, CACurrentMediaTime, CAKeyframeAnimation, CALayer, CAMediaTiming as _,
        CAMetalLayer, CATransaction, kCAAnimationDiscrete,
    };

    use crate::cursor_blink::{self, BlinkAnimation, LayerBlinkPlan};

    fn blink_key() -> &'static NSString {
        ns_string!("kettle.cursor-blink")
    }

    /// Apply `change` in one transaction with implicit actions disabled, and
    /// flush so it commits now rather than at the next run-loop wake.
    fn transaction<R>(change: impl FnOnce() -> R) -> R {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let result = change();
        CATransaction::commit();
        CATransaction::flush();
        result
    }

    pub(super) struct Layer {
        root: Retained<CALayer>,
        main: Retained<CAMetalLayer>,
        patch: Retained<CAMetalLayer>,
    }

    impl Layer {
        pub(super) fn install(window: &winit::window::Window) -> Result<Self, &'static str> {
            use winit::raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

            if objc2_foundation_03::MainThreadMarker::new().is_none() {
                return Err("not on the main thread");
            }
            let handle = window.window_handle().map_err(|_| "no window handle")?;
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                return Err("not an AppKit window");
            };
            // SAFETY: winit's live content view, used on the main thread.
            let view: &AnyObject = unsafe { appkit.ns_view.cast::<AnyObject>().as_ref() };
            // SAFETY: `-[NSView layer]` returns an optional CALayer; this is
            // how raw-window-metal reads it before wgpu inserts its layer.
            let root: Option<Retained<CALayer>> = unsafe { objc2_06::msg_send![view, layer] };
            Self::install_on(root.ok_or("the view has no layer")?)
        }

        pub(super) fn install_on(root: Retained<CALayer>) -> Result<Self, &'static str> {
            // SAFETY: a layer's `sublayers` is an array of CALayer.
            let sublayers = unsafe { root.sublayers() }.ok_or("the view has no Metal layer")?;
            // wgpu adds its layer with `addSublayer`, and a rebuilt surface
            // adds a new one above the old, so the topmost is the live one.
            let main = sublayers
                .to_vec()
                .into_iter()
                .rev()
                .find_map(|layer| layer.downcast::<CAMetalLayer>().ok())
                .ok_or("the view has no Metal layer")?;
            let patch = CAMetalLayer::new();
            transaction(|| {
                patch.setHidden(true);
                patch.setOpaque(false);
                patch.setContentsScale(main.contentsScale());
                root.insertSublayer_above(&patch, Some(&**main));
            });
            Ok(Self { root, main, patch })
        }

        fn surface_target(&self) -> NonNull<c_void> {
            NonNull::from(&*self.patch).cast()
        }

        pub(super) fn attach(
            &self,
            renderer: &mut kettle_render::Renderer,
        ) -> Result<(), &'static str> {
            // SAFETY: the pointer is this live CAMetalLayer, and wgpu retains
            // it for as long as the surface lives.
            unsafe { renderer.attach_cursor_layer(self.surface_target()) }
                .map_err(|_| "the cursor surface could not be configured")
        }

        pub(super) fn show(
            &self,
            rect_px: [u32; 4],
            scale: f64,
            plan: &LayerBlinkPlan,
            now: Instant,
        ) -> Result<(), &'static str> {
            // The patch maps 1:1 onto device pixels only at the Metal layer's
            // own scale; a scale change ends the blink before a mismatch shows.
            if (self.main.contentsScale() - scale).abs() > 1e-9 {
                return Err("the window's scale does not match its Metal layer");
            }
            if rect_px[2] == 0 || rect_px[3] == 0 {
                return Err("empty cursor patch");
            }
            let flipped = self.root.isGeometryFlipped();
            let [x, y, w, h] = cursor_blink::layer_frame_points(
                rect_px,
                scale,
                self.root.bounds().size.height,
                flipped,
            );
            let blink = keyframes(&plan.animation(now, CACurrentMediaTime()), &self.patch);
            transaction(|| {
                self.patch.removeAnimationForKey(blink_key());
                self.patch.setContentsScale(scale);
                self.patch
                    .setFrame(CGRect::new(CGPoint::new(x, y), CGSize::new(w, h)));
                self.patch.setAutoresizingMask(CAAutoresizingMask(
                    cursor_blink::pinned_autoresizing_mask(flipped),
                ));
                // The model value is visible, so the cursor rests visible when
                // the animation ends, as the GPU blink stops on its visible
                // phase, with no wake.
                self.patch.setOpacity(1.0);
                self.patch.addAnimation_forKey(&blink, Some(blink_key()));
                self.patch.setHidden(false);
            });
            Ok(())
        }

        pub(super) fn hide(&self) {
            transaction(|| {
                self.patch.removeAnimationForKey(blink_key());
                self.patch.setHidden(true);
            });
        }

        pub(super) fn begin_exit_frame(&self) {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            // wgpu reads this when it acquires the drawable, so it applies to
            // this frame only.
            self.main.setPresentsWithTransaction(true);
        }

        pub(super) fn end_exit_frame(&self) {
            self.main.setPresentsWithTransaction(false);
            CATransaction::commit();
            CATransaction::flush();
        }
    }

    impl Drop for Layer {
        fn drop(&mut self) {
            transaction(|| {
                self.patch.removeAnimationForKey(blink_key());
                self.patch.removeFromSuperlayer();
                self.main.setPresentsWithTransaction(false);
            });
        }
    }

    /// The discrete opacity animation for `anim`, timed on `layer`'s clock.
    fn keyframes(anim: &BlinkAnimation, layer: &CALayer) -> Retained<CAKeyframeAnimation> {
        let blink = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
        let values = NSArray::from_retained_slice(&anim.values.map(NSNumber::new_f32));
        // SAFETY: the array holds NSNumbers, which are objects.
        unsafe { blink.setValues(Some(values.cast_unchecked::<AnyObject>())) };
        let key_times = NSArray::from_retained_slice(&anim.key_times.map(NSNumber::new_f32));
        blink.setKeyTimes(Some(&*key_times));
        // SAFETY: reads an immutable framework constant.
        blink.setCalculationMode(unsafe { kCAAnimationDiscrete });
        blink.setDuration(anim.duration);
        blink.setRepeatCount(anim.repeat_count);
        blink.setBeginTime(layer.convertTime_fromLayer(anim.begin_time, None));
        blink.setRemovedOnCompletion(true);
        blink
    }

    #[cfg(test)]
    mod tests {
        use std::time::{Duration, Instant};

        use objc2_06::rc::Retained;
        use objc2_quartz_core_03::{
            CAKeyframeAnimation, CALayer, CAMediaTiming as _, CAMetalLayer, CATransaction,
            kCAAnimationDiscrete,
        };

        use super::{Layer, blink_key, transaction};
        use crate::cursor_blink::layer_plan;

        /// Core Animation's commit on one test thread can drop a finished
        /// animation from a windowless layer another thread is reading, so
        /// these tests take turns.
        static CA: std::sync::Mutex<()> = std::sync::Mutex::new(());

        fn serial() -> std::sync::MutexGuard<'static, ()> {
            CA.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
        }

        /// A root layer holding a stand-in for wgpu's Metal layer at 2x, and
        /// an older Metal layer beneath it, as a rebuilt surface leaves one.
        fn tree() -> (
            Retained<CALayer>,
            Retained<CAMetalLayer>,
            Retained<CAMetalLayer>,
        ) {
            let root = CALayer::new();
            let stale = CAMetalLayer::new();
            let main = CAMetalLayer::new();
            transaction(|| {
                root.setGeometryFlipped(true);
                main.setContentsScale(2.0);
                root.addSublayer(&stale);
                root.addSublayer(&main);
            });
            (root, stale, main)
        }

        #[test]
        fn cursor_layer_sits_directly_above_the_metal_layer() {
            let _serial = serial();
            let (root, stale, main) = tree();
            let layer = Layer::install_on(root.clone()).expect("install");
            // SAFETY: sublayers of a CALayer.
            let order = unsafe { root.sublayers() }.expect("sublayers").to_vec();
            assert_eq!(order.len(), 3);
            assert_eq!(&*order[0], &**stale);
            assert_eq!(&*order[1], &**main);
            assert_eq!(&*order[2], &**layer.patch);
            assert!(layer.patch.isHidden());
            assert!(!layer.patch.isOpaque());
            assert_eq!(layer.patch.contentsScale(), 2.0);
            drop(layer);
            // SAFETY: as above.
            let after = unsafe { root.sublayers() }.expect("sublayers").to_vec();
            assert_eq!(after.len(), 2, "dropping the layer removes it");
        }

        #[test]
        fn show_installs_a_discrete_opacity_blink() {
            let _serial = serial();
            let (root, _stale, _main) = tree();
            let layer = Layer::install_on(root).expect("install");
            let now = Instant::now();
            let plan = layer_plan(
                false,
                now,
                now - Duration::from_millis(530),
                Duration::from_millis(530),
                Some(Duration::from_secs(10)),
            )
            .expect("plan");
            // A layer tree with no window drops a finished-on-completion
            // animation when its transaction commits, since nothing renders
            // it. Hold the commit open to read what `show` installed.
            CATransaction::begin();
            layer.show([11, 21, 17, 33], 2.0, &plan, now).expect("show");
            assert!(!layer.patch.isHidden());
            assert_eq!(layer.patch.opacity(), 1.0, "the model value rests visible");
            let frame = layer.patch.frame();
            assert_eq!(
                (
                    frame.origin.x,
                    frame.origin.y,
                    frame.size.width,
                    frame.size.height
                ),
                (5.5, 10.5, 8.5, 16.5)
            );
            // One animation, the blink. (A windowless tree adds no implicit
            // actions either way, so the source guard covers
            // `setDisableActions`.)
            let keys = layer
                .patch
                .animationKeys()
                .expect("animation keys")
                .to_vec();
            assert_eq!(keys.len(), 1, "animations: {keys:?}");
            assert_eq!(&*keys[0], blink_key());
            // SAFETY: the key names the animation `show` added.
            let added = unsafe { layer.patch.animationForKey(blink_key()) }.expect("blink");
            let blink = added
                .downcast::<CAKeyframeAnimation>()
                .expect("a keyframe animation");
            assert_eq!(blink.keyPath().expect("key path").to_string(), "opacity");
            // SAFETY: reads an immutable framework constant.
            assert_eq!(&*blink.calculationMode(), unsafe { kCAAnimationDiscrete });
            let values: Vec<f64> = blink
                .values()
                .expect("values")
                .to_vec()
                .into_iter()
                .map(|value| {
                    value
                        .downcast::<objc2_foundation_03::NSNumber>()
                        .expect("a number")
                        .as_f64()
                })
                .collect();
            assert_eq!(values, [0.0, 1.0]);
            let key_times: Vec<f64> = blink
                .keyTimes()
                .expect("key times")
                .to_vec()
                .iter()
                .map(|time| time.as_f64())
                .collect();
            assert_eq!(key_times, [0.0, 0.5, 1.0]);
            assert!((blink.duration() - 1.06).abs() < 1e-9);
            // Off edges from here before the 10 s stop, 530 ms after activity.
            assert_eq!(blink.repeatCount(), 8.5);
            assert!(blink.isRemovedOnCompletion());
            CATransaction::commit();
        }

        #[test]
        fn hide_leaves_no_animation() {
            let _serial = serial();
            let (root, _stale, _main) = tree();
            let layer = Layer::install_on(root).expect("install");
            let now = Instant::now();
            let plan = layer_plan(false, now, now, Duration::from_millis(500), None).expect("plan");
            // Held open as in `show_installs_a_discrete_opacity_blink`.
            CATransaction::begin();
            layer.show([0, 0, 8, 16], 2.0, &plan, now).expect("show");
            assert!(
                layer
                    .patch
                    .animationKeys()
                    .is_some_and(|keys| keys.count() == 1)
            );
            layer.hide();
            assert!(layer.patch.isHidden());
            assert!(
                layer
                    .patch
                    .animationKeys()
                    .is_none_or(|keys| keys.to_vec().is_empty())
            );
            CATransaction::commit();
        }

        #[test]
        fn presents_with_transaction_is_restored() {
            let _serial = serial();
            let (root, _stale, main) = tree();
            let layer = Layer::install_on(root).expect("install");
            assert!(!main.presentsWithTransaction());
            layer.begin_exit_frame();
            assert!(
                main.presentsWithTransaction(),
                "the exit frame presents in step"
            );
            layer.hide();
            layer.end_exit_frame();
            assert!(!main.presentsWithTransaction());
            assert!(layer.patch.isHidden());
        }

        #[test]
        fn a_late_frame_does_not_show_a_stale_patch() {
            let _serial = serial();
            let (root, _stale, _main) = tree();
            let layer = crate::macos_cursor_layer::CursorLayer {
                imp: Layer::install_on(root).expect("install"),
            };
            let edge = Instant::now();
            let plan =
                layer_plan(false, edge, edge, Duration::from_millis(500), None).expect("plan");
            assert!(
                layer
                    .show([0, 0, 8, 16], 2.0, &plan, edge + Duration::from_millis(251))
                    .is_err()
            );
            assert!(layer.imp.patch.isHidden());
        }

        #[test]
        fn a_scale_change_or_an_empty_patch_refuses_to_show() {
            let _serial = serial();
            let (root, _stale, _main) = tree();
            let layer = Layer::install_on(root).expect("install");
            let now = Instant::now();
            let plan = layer_plan(false, now, now, Duration::from_millis(500), None).expect("plan");
            assert!(layer.show([0, 0, 8, 16], 1.0, &plan, now).is_err());
            assert!(layer.show([0, 0, 0, 16], 2.0, &plan, now).is_err());
            assert!(layer.patch.isHidden());
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::time::Instant;

    use crate::cursor_blink::LayerBlinkPlan;

    /// No cursor layer exists off macOS.
    pub(super) enum Layer {}

    impl Layer {
        pub(super) fn install(_window: &winit::window::Window) -> Result<Self, &'static str> {
            Err("the cursor layer is macOS-only")
        }

        pub(super) fn attach(
            &self,
            _renderer: &mut kettle_render::Renderer,
        ) -> Result<(), &'static str> {
            match *self {}
        }

        pub(super) fn show(
            &self,
            _rect_px: [u32; 4],
            _scale: f64,
            _plan: &LayerBlinkPlan,
            _now: Instant,
        ) -> Result<(), &'static str> {
            match *self {}
        }

        pub(super) fn hide(&self) {
            match *self {}
        }

        pub(super) fn begin_exit_frame(&self) {
            match *self {}
        }

        pub(super) fn end_exit_frame(&self) {
            match *self {}
        }
    }
}

#[cfg(test)]
mod source_tests {
    /// Every layer mutation runs with implicit actions disabled: inside
    /// `transaction`, or in the exit frame's own transaction, which
    /// `begin_exit_frame` opens with actions off and `end_exit_frame`
    /// commits. Nothing registers a delegate or a completion block. A
    /// windowless test tree adds no implicit animations, so only this guard
    /// can catch a missing `setDisableActions`.
    #[test]
    fn every_layer_mutation_disables_implicit_actions() {
        let src = kettle_test_support::production_source(include_str!("macos_cursor_layer.rs"));
        let imp = src
            .split_once("#[cfg(target_os = \"macos\")]\nmod imp {")
            .expect("the macOS imp")
            .1
            .split_once("#[cfg(not(target_os = \"macos\"))]")
            .expect("end of the macOS imp")
            .0;
        let transaction = imp
            .split_once("fn transaction<R>(")
            .expect("transaction helper")
            .1
            .split_once("pub(super) struct Layer")
            .expect("end of transaction helper")
            .0;
        assert!(transaction.contains("CATransaction::setDisableActions(true);"));
        let mutators = [
            "setHidden(",
            "setOpaque(",
            "setContentsScale(",
            "setFrame(",
            "setAutoresizingMask(",
            "setOpacity(",
            "insertSublayer_above(",
            "addAnimation_forKey(",
            "removeAnimationForKey(",
            "removeFromSuperlayer(",
            "setPresentsWithTransaction(",
        ];
        let mut function = "";
        let mut guarded = false;
        let mut checked = 0;
        for line in imp.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed
                .strip_prefix("pub(super) fn ")
                .or_else(|| trimmed.strip_prefix("fn "))
            {
                function = rest.split('(').next().unwrap_or(rest);
                // `end_exit_frame` finishes the transaction `begin_exit_frame`
                // opened with actions disabled.
                guarded = function == "end_exit_frame";
            }
            if trimmed.contains("transaction(|| {")
                || trimmed.contains("CATransaction::setDisableActions(true);")
            {
                guarded = true;
            }
            if mutators.iter().any(|mutator| trimmed.contains(mutator)) {
                checked += 1;
                assert!(
                    guarded,
                    "{function} mutates a layer outside a transaction: {trimmed}"
                );
            }
        }
        assert!(checked >= 12, "the guard found only {checked} mutations");
        assert!(!imp.contains("setDelegate") && !imp.contains("setCompletionBlock"));
    }
}
