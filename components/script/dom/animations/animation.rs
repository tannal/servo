/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

use dom_struct::dom_struct;
use js::context::JSContext;
use js::rust::HandleObject;
use script_bindings::cell::DomRefCell;
use script_bindings::codegen::GenericBindings::AnimationBinding::{
    AnimationPlayState, AnimationReplaceState,
};
use script_bindings::codegen::GenericBindings::AnimationTimelineBinding::AnimationTimelineMethods;
use script_bindings::codegen::GenericBindings::DocumentBinding::DocumentMethods;
use script_bindings::codegen::GenericBindings::EventBinding::EventInit;
use script_bindings::codegen::GenericBindings::WindowBinding::WindowMethods;
use script_bindings::error::{Error, ErrorResult};
use script_bindings::inheritance::Castable;
use script_bindings::num::Finite;
use script_bindings::reflector::reflect_dom_object_with_proto;
use script_bindings::root::{Dom, DomRoot};
use script_bindings::str::DOMString;
use stylo_atoms::Atom;

use crate::dom::animationeffect::AnimationEffect;
use crate::dom::bindings::codegen::Bindings::AnimationBinding::AnimationMethods;
use crate::dom::bindings::codegen::Bindings::AnimationPlaybackEventBinding::AnimationPlaybackEventInit;
use crate::dom::bindings::reflector::DomGlobal;
use crate::dom::bindings::root::MutNullableDom;
use crate::dom::event::Event;
use crate::dom::event::animationplaybackevent::AnimationPlaybackEvent;
use crate::dom::eventtarget::EventTarget;
use crate::dom::globalscope::GlobalScope;
use crate::dom::keyframeeffect::KeyframeEffect;
use crate::dom::promise::{Promise, RootedPromise, TracedPromise};
use crate::dom::types::AnimationTimeline;
use crate::dom::window::Window;
use crate::realms::enter_auto_realm;
use crate::runtime::job_queue::MicrotaskRunnable;

static NEXT_ANIMATION_SEQUENCE_NUMBER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Default, Eq, MallocSizeOf, PartialEq)]
enum PendingTask {
    #[default]
    None,
    Play,
    Pause,
}

#[derive(JSTraceable, MallocSizeOf)]
#[cfg_attr(crown, crown::unrooted_must_root_lint::must_root)]
struct FinishNotificationMicrotask {
    animation: Dom<Animation>,
}

impl MicrotaskRunnable for FinishNotificationMicrotask {
    fn handler(&self, cx: &mut JSContext) {
        let mut realm = enter_auto_realm(cx, &*self.animation);
        let cx = &mut realm;
        self.animation.perform_finish_notification_steps(cx);
    }
}

/// <https://drafts.csswg.org/web-animations-1/#animation>
#[dom_struct]
pub(crate) struct Animation {
    event_target: EventTarget,

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-id>
    id: DomRefCell<DOMString>,

    /// <https://drafts.csswg.org/web-animations-1/#timeline>
    timeline: MutNullableDom<AnimationTimeline>,

    /// <https://drafts.csswg.org/web-animations-1/#animation-associated-effect>
    associated_effect: MutNullableDom<AnimationEffect>,

    /// <https://drafts.csswg.org/web-animations-1/#animation-start-time>
    start_time: Cell<Option<f64>>,

    /// <https://drafts.csswg.org/web-animations-1/#animation-hold-time>
    hold_time: Cell<Option<f64>>,

    /// <https://drafts.csswg.org/web-animations-1/#animation-playback-rate>
    playback_rate: Cell<f64>,

    /// <https://drafts.csswg.org/web-animations-1/#pending-playback-rate>
    pending_playback_rate: Cell<Option<f64>>,

    /// Pending play or pause task.
    #[no_trace]
    pending_task: Cell<PendingTask>,

    /// <https://drafts.csswg.org/web-animations-1/#animation-replace-state>
    replace_state: Cell<AnimationReplaceState>,

    /// <https://drafts.csswg.org/web-animations-1/#current-ready-promise>
    ready_promise: DomRefCell<TracedPromise>,

    /// <https://drafts.csswg.org/web-animations-1/#current-finished-promise>
    finished_promise: DomRefCell<TracedPromise>,

    /// Whether `finished_promise` has already been resolved.
    finished_promise_is_resolved: Cell<bool>,

    /// Whether a finish notification microtask is already queued.
    pending_finish_microtask: Cell<bool>,

    /// Monotonically increasing sequence number for composite ordering.
    sequence_number: u64,
}

impl Animation {
    fn new_inherited(
        ready_promise: &RootedPromise,
        finished_promise: &RootedPromise,
    ) -> Self {
        Self {
            event_target: EventTarget::new_inherited(),
            id: DomRefCell::new(DOMString::new()),
            timeline: Default::default(),
            associated_effect: Default::default(),
            start_time: Cell::new(None),
            hold_time: Cell::new(None),
            playback_rate: Cell::new(1.0),
            pending_playback_rate: Cell::new(None),
            pending_task: Cell::new(PendingTask::None),
            replace_state: Cell::new(AnimationReplaceState::Active),
            ready_promise: DomRefCell::new(ready_promise.to_traced()),
            finished_promise: DomRefCell::new(finished_promise.to_traced()),
            finished_promise_is_resolved: Cell::new(false),
            pending_finish_microtask: Cell::new(false),
            sequence_number: NEXT_ANIMATION_SEQUENCE_NUMBER.fetch_add(1, Ordering::Relaxed),
        }
    }

    fn new_with_proto(
        cx: &mut JSContext,
        global: &GlobalScope,
        proto: Option<HandleObject>,
    ) -> DomRoot<Self> {
        let ready_promise = Promise::new(cx, global);
        let finished_promise = Promise::new(cx, global);
        let animation = reflect_dom_object_with_proto(
            cx,
            Box::new(Self::new_inherited(&ready_promise, &finished_promise)),
            global,
            proto,
        );
        // An animation's initial ready promise is already resolved with the animation itself.
        ready_promise.resolve_native(cx, &*animation);
        animation
    }

    pub(crate) fn sequence_number(&self) -> u64 {
        self.sequence_number
    }

    fn timeline_current_time(&self) -> Option<f64> {
        self.timeline
            .get()
            .and_then(|timeline| timeline.GetCurrentTime().map(|t| *t))
    }

    fn associated_effect_end(&self) -> f64 {
        self.associated_effect
            .get()
            .map_or(0.0, |effect| effect.end_time())
    }

    fn effective_playback_rate(&self) -> f64 {
        self.pending_playback_rate
            .get()
            .unwrap_or_else(|| self.playback_rate.get())
    }

    fn apply_pending_playback_rate(&self) {
        if let Some(rate) = self.pending_playback_rate.take() {
            self.playback_rate.set(rate);
        }
    }

    /// <https://drafts.csswg.org/web-animations-1/#current-time>
    pub(crate) fn current_time(&self) -> Option<f64> {
        if let Some(hold_time) = self.hold_time.get() {
            return Some(hold_time);
        }
        let timeline_time = self.timeline_current_time()?;
        let start_time = self.start_time.get()?;
        Some((timeline_time - start_time) * self.playback_rate.get())
    }

    /// <https://drafts.csswg.org/web-animations-1/#silently-set-the-current-time>
    fn silently_set_current_time(&self, seek_time: Option<f64>) {
        let Some(seek_time) = seek_time else {
            return;
        };
        let timeline_time = self.timeline_current_time();
        let rate = self.playback_rate.get();
        if self.hold_time.get().is_some() ||
            self.start_time.get().is_none() ||
            timeline_time.is_none() ||
            rate == 0.0
        {
            self.hold_time.set(Some(seek_time));
        } else if let Some(timeline_time) = timeline_time {
            self.start_time.set(Some(timeline_time - seek_time / rate));
        }
        if timeline_time.is_none() {
            self.start_time.set(None);
        }
    }

    fn register_with_document(&self) {
        if let Some(window) = DomRoot::downcast::<Window>(self.global()) {
            let document = window.Document();
            document
                .animation_manager()
                .register_web_animation(&window, self);
        }
    }

    /// <https://drafts.csswg.org/web-animations-1/#relevant-animations>
    pub(crate) fn is_relevant(&self) -> bool {
        if self.replace_state.get() == AnimationReplaceState::Removed {
            return false;
        }
        let state = self.PlayState();
        match state {
            AnimationPlayState::Running | AnimationPlayState::Paused => true,
            AnimationPlayState::Idle => false,
            AnimationPlayState::Finished => self
                .associated_effect
                .get()
                .is_some_and(|effect| effect.is_in_effect(self.current_time())),
        }
    }

    /// Whether this animation needs frame ticks from the document's animation loop.
    pub(crate) fn needs_tick(&self) -> bool {
        self.pending_task.get() != PendingTask::None ||
            self.PlayState() == AnimationPlayState::Running
    }

    /// Returns the target `KeyframeEffect` if the associated effect is a `KeyframeEffect`.
    pub(crate) fn keyframe_effect(&self) -> Option<DomRoot<KeyframeEffect>> {
        self.associated_effect
            .get()
            .and_then(|effect| DomRoot::downcast::<KeyframeEffect>(effect))
    }

    /// Called on each animation frame tick from `AnimationManager`.
    pub(crate) fn tick(&self, cx: &mut JSContext) {
        if let Some(ready_time) = self.timeline_current_time() {
            match self.pending_task.get() {
                PendingTask::Play => {
                    self.apply_pending_playback_rate();
                    let rate = self.playback_rate.get();
                    if let Some(hold_time) = self.hold_time.get() {
                        if rate == 0.0 {
                            self.start_time.set(Some(ready_time));
                        } else {
                            self.start_time.set(Some(ready_time - hold_time / rate));
                            self.hold_time.set(None);
                        }
                    } else if self.start_time.get().is_none() {
                        self.start_time.set(Some(ready_time));
                    }
                    self.pending_task.set(PendingTask::None);
                    let ready = self.ready_promise.borrow().root(cx);
                    ready.resolve_native(cx, self);
                },
                PendingTask::Pause => {
                    self.apply_pending_playback_rate();
                    if self.hold_time.get().is_none() {
                        if let Some(start_time) = self.start_time.get() {
                            self.hold_time
                                .set(Some((ready_time - start_time) * self.playback_rate.get()));
                        }
                    }
                    self.start_time.set(None);
                    self.pending_task.set(PendingTask::None);
                    let ready = self.ready_promise.borrow().root(cx);
                    ready.resolve_native(cx, self);
                },
                PendingTask::None => {},
            }
        }

        self.update_finished_state(cx, false, false);
    }

    /// <https://drafts.csswg.org/web-animations-1/#updating-the-finished-state>
    fn update_finished_state(
        &self,
        cx: &mut JSContext,
        did_seek: bool,
        synchronously_notify: bool,
    ) {
        // Step 1. Calculate the unconstrained current time.
        let unconstrained_current_time = if did_seek {
            self.current_time()
        } else if let Some(hold_time) = self.hold_time.get() {
            Some(hold_time)
        } else {
            self.timeline_current_time()
                .zip(self.start_time.get())
                .map(|(tl, st)| (tl - st) * self.playback_rate.get())
        };

        let end = self.associated_effect_end();
        let rate = self.playback_rate.get();

        // Step 2 & 3. Clamp hold time when finished, or clear hold time when moving back in range.
        if let Some(t) = unconstrained_current_time {
            if self.start_time.get().is_some() && self.pending_task.get() == PendingTask::None {
                if rate > 0.0 && t >= end {
                    let hold = if did_seek { t } else { end };
                    self.hold_time.set(Some(hold));
                } else if rate < 0.0 && t <= 0.0 {
                    let hold = if did_seek { t } else { 0.0 };
                    self.hold_time.set(Some(hold));
                } else if rate != 0.0 {
                    if let Some(timeline_time) = self.timeline_current_time() {
                        if did_seek && self.hold_time.get().is_some() {
                            self.start_time.set(Some(timeline_time - t / rate));
                        }
                        self.hold_time.set(None);
                    }
                }
            }
        }

        // Step 4 & 5. Check play state for finished notification.
        if self.PlayState() == AnimationPlayState::Finished {
            if !self.finished_promise_is_resolved.get() {
                if synchronously_notify {
                    self.perform_finish_notification_steps(cx);
                } else if !self.pending_finish_microtask.get() {
                    self.pending_finish_microtask.set(true);
                    self.global().enqueue_microtask(
                        cx,
                        Box::new(FinishNotificationMicrotask {
                            animation: Dom::from_ref(self),
                        }),
                    );
                }
            }
        } else {
            self.pending_finish_microtask.set(false);
            if self.finished_promise_is_resolved.get() {
                let new_promise = Promise::new(cx, &self.global());
                *self.finished_promise.borrow_mut() = new_promise.to_traced();
                self.finished_promise_is_resolved.set(false);
            }
        }
    }

    /// <https://drafts.csswg.org/web-animations-1/#finish-notification-steps>
    fn perform_finish_notification_steps(&self, cx: &mut JSContext) {
        self.pending_finish_microtask.set(false);
        if self.PlayState() != AnimationPlayState::Finished ||
            self.finished_promise_is_resolved.get()
        {
            return;
        }

        self.finished_promise_is_resolved.set(true);
        let finished = self.finished_promise.borrow().root(cx);
        finished.resolve_native(cx, self);

        self.queue_playback_event(
            Atom::from("finish"),
            self.current_time(),
            self.timeline_current_time(),
        );
    }

    fn queue_playback_event(
        &self,
        event_type: Atom,
        current_time: Option<f64>,
        timeline_time: Option<f64>,
    ) {
        let Some(window) = DomRoot::downcast::<Window>(self.global()) else {
            return;
        };
        let document = window.Document();
        document.animation_manager().queue_web_animation_event(
            &window,
            self,
            event_type,
            current_time,
            timeline_time,
        );
    }

    pub(crate) fn dispatch_playback_event(
        &self,
        cx: &mut JSContext,
        window: &Window,
        event_type: Atom,
        current_time: Option<f64>,
        timeline_time: Option<f64>,
    ) {
        let init = AnimationPlaybackEventInit {
            parent: EventInit {
                bubbles: false,
                cancelable: false,
                composed: false,
            },
            currentTime: current_time.and_then(Finite::new),
            timelineTime: timeline_time.and_then(Finite::new),
        };
        let event = AnimationPlaybackEvent::new(cx, window, event_type, &init);
        event.upcast::<Event>().set_trusted(true);
        event.upcast::<Event>().fire(cx, self.upcast());
    }

    /// <https://drafts.csswg.org/web-animations-1/#play-an-animation>
    pub(crate) fn play_animation(&self, cx: &mut JSContext, auto_rewind: bool) -> ErrorResult {
        let aborted_pause = self.pending_task.get() == PendingTask::Pause;
        let has_pending_ready_promise = self.pending_task.get() != PendingTask::None;

        let effective_rate = self.effective_playback_rate();
        let current_time = self.current_time();
        let end = self.associated_effect_end();

        let mut seek_time = None;
        if auto_rewind {
            if effective_rate >= 0.0 &&
                (current_time.is_none() ||
                    current_time.is_some_and(|t| t < 0.0 || t >= end))
            {
                seek_time = Some(0.0);
            } else if effective_rate < 0.0 &&
                (current_time.is_none() ||
                    current_time.is_some_and(|t| t <= 0.0 || t > end))
            {
                if end.is_infinite() {
                    return Err(Error::InvalidState(None));
                }
                seek_time = Some(end);
            }
        } else if current_time.is_none() {
            seek_time = Some(0.0);
        }

        if seek_time.is_some() {
            self.hold_time.set(seek_time);
        }

        if self.hold_time.get().is_some() {
            self.start_time.set(None);
        }

        if self.hold_time.get().is_none() &&
            seek_time.is_none() &&
            !aborted_pause &&
            self.pending_playback_rate.get().is_none()
        {
            return Ok(());
        }

        if !has_pending_ready_promise {
            let new_ready = Promise::new(cx, &self.global());
            *self.ready_promise.borrow_mut() = new_ready.to_traced();
        }

        self.pending_task.set(PendingTask::Play);
        self.register_with_document();
        self.update_finished_state(cx, false, false);
        Ok(())
    }

    /// <https://drafts.csswg.org/web-animations-1/#animation-set-the-timeline-of-an-animation>
    fn set_the_timeline(&self, cx: &mut JSContext, timeline: Option<&AnimationTimeline>) {
        let old_timeline = self.timeline.get();
        if old_timeline.as_deref() == timeline {
            return;
        }
        let previous_current_time = self.current_time();
        self.timeline.set(timeline);
        if timeline.is_some() {
            if self.start_time.get().is_some() {
                self.hold_time.set(None);
            }
        } else {
            self.hold_time.set(previous_current_time);
            self.start_time.set(None);
        }
        self.update_finished_state(cx, false, false);
    }

    /// <https://drafts.csswg.org/web-animations-1/#animation-set-the-associated-effect-of-an-animation>
    fn set_the_associated_effect(&self, cx: &mut JSContext, effect: Option<&AnimationEffect>) {
        self.associated_effect.set(effect);
        self.update_finished_state(cx, false, false);
    }
}

impl AnimationMethods<crate::DomTypeHolder> for Animation {
    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-animation>
    fn Constructor(
        cx: &mut JSContext,
        window: &Window,
        proto: Option<HandleObject>,
        effect: Option<&AnimationEffect>,
        timeline: Option<Option<&AnimationTimeline>>,
    ) -> DomRoot<Self> {
        // Step 1. Let animation be a new Animation object.
        let animation = Animation::new_with_proto(cx, window.upcast(), proto);

        // Step 2. Run the procedure to set the timeline of an animation on animation passing timeline
        // as the new timeline; or, if the timeline argument is missing, passing the default document
        // timeline of the Document associated with the Window that is the current global object.
        match timeline {
            Some(timeline) => animation.set_the_timeline(cx, timeline),
            None => {
                let document = window.document_unrooted(cx.no_gc());
                let timeline = document.Timeline();
                animation.set_the_timeline(cx, Some(timeline.upcast()));
            },
        }

        // Step 3. Run the procedure to set the associated effect of an animation on animation passing
        // source as the new effect.
        animation.set_the_associated_effect(cx, effect);

        animation
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-id>
    fn Id(&self) -> DOMString {
        self.id.borrow().clone()
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-id>
    fn SetId(&self, id: DOMString) {
        *self.id.borrow_mut() = id;
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-effect>
    fn GetEffect(&self) -> Option<DomRoot<AnimationEffect>> {
        self.associated_effect.get()
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-effect>
    fn SetEffect(&self, cx: &mut JSContext, effect: Option<&AnimationEffect>) {
        self.set_the_associated_effect(cx, effect);
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-animation-effect-timeline-timeline>
    fn GetTimeline(&self) -> Option<DomRoot<AnimationTimeline>> {
        self.timeline.get()
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-animation-effect-timeline-timeline>
    fn SetTimeline(&self, cx: &mut JSContext, timeline: Option<&AnimationTimeline>) {
        self.set_the_timeline(cx, timeline);
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-starttime>
    fn GetStartTime(&self) -> Option<Finite<f64>> {
        self.start_time.get().and_then(Finite::new)
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-starttime>
    fn SetStartTime(&self, cx: &mut JSContext, new_start_time: Option<Finite<f64>>) {
        let new_start_time = new_start_time.map(|v| *v);
        let timeline_time = self.timeline_current_time();
        if timeline_time.is_none() && new_start_time.is_some() {
            self.hold_time.set(None);
        }
        let previous_current_time = self.current_time();
        self.apply_pending_playback_rate();
        self.start_time.set(new_start_time);
        if new_start_time.is_some() {
            if self.playback_rate.get() != 0.0 {
                self.hold_time.set(None);
            }
        } else {
            self.hold_time.set(previous_current_time);
        }
        if self.pending_task.get() != PendingTask::None {
            self.pending_task.set(PendingTask::None);
            let ready = self.ready_promise.borrow().root(cx);
            ready.resolve_native(cx, self);
        }
        self.register_with_document();
        self.update_finished_state(cx, true, false);
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-currenttime>
    fn GetCurrentTime(&self) -> Option<Finite<f64>> {
        self.current_time().and_then(Finite::new)
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-currenttime>
    fn SetCurrentTime(&self, cx: &mut JSContext, new_time: Option<Finite<f64>>) -> ErrorResult {
        let Some(seek_time) = new_time.map(|v| *v) else {
            if self.current_time().is_some() {
                return Err(Error::Type(
                    c"Cannot set currentTime to null when currentTime is not null".to_owned(),
                ));
            }
            return Ok(());
        };

        self.silently_set_current_time(Some(seek_time));

        if self.pending_task.get() == PendingTask::Pause {
            self.hold_time.set(Some(seek_time));
            self.apply_pending_playback_rate();
            self.start_time.set(None);
            self.pending_task.set(PendingTask::None);
            let ready = self.ready_promise.borrow().root(cx);
            ready.resolve_native(cx, self);
        }

        self.update_finished_state(cx, true, false);
        Ok(())
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-playbackrate>
    fn PlaybackRate(&self) -> Finite<f64> {
        Finite::wrap(self.playback_rate.get())
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-playbackrate>
    fn SetPlaybackRate(&self, cx: &mut JSContext, new_rate: Finite<f64>) {
        self.pending_playback_rate.set(None);
        let previous_time = self.current_time();
        self.playback_rate.set(*new_rate);
        if previous_time.is_some() {
            self.silently_set_current_time(previous_time);
        }
        self.update_finished_state(cx, false, false);
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-playstate>
    fn PlayState(&self) -> AnimationPlayState {
        let current_time = self.current_time();
        if current_time.is_none() &&
            self.start_time.get().is_none() &&
            self.pending_task.get() == PendingTask::None
        {
            return AnimationPlayState::Idle;
        }
        if self.pending_task.get() == PendingTask::Pause ||
            (self.start_time.get().is_none() && self.pending_task.get() != PendingTask::Play)
        {
            return AnimationPlayState::Paused;
        }
        if let Some(current_time) = current_time {
            let rate = self.playback_rate.get();
            let end = self.associated_effect_end();
            if (rate > 0.0 && current_time >= end) || (rate < 0.0 && current_time <= 0.0) {
                return AnimationPlayState::Finished;
            }
        }
        AnimationPlayState::Running
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-replacestate>
    fn ReplaceState(&self) -> AnimationReplaceState {
        self.replace_state.get()
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-pending>
    fn Pending(&self) -> bool {
        self.pending_task.get() != PendingTask::None
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-ready>
    fn Ready(&self, cx: &mut JSContext) -> RootedPromise {
        self.ready_promise.borrow().root(cx)
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-finished>
    fn Finished(&self, cx: &mut JSContext) -> RootedPromise {
        self.finished_promise.borrow().root(cx)
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-cancel>
    fn Cancel(&self, cx: &mut JSContext) {
        if self.PlayState() != AnimationPlayState::Idle {
            if self.pending_task.get() != PendingTask::None {
                self.pending_task.set(PendingTask::None);
                self.pending_playback_rate.set(None);
                let old_ready = self.ready_promise.borrow().root(cx);
                old_ready.reject_error(cx, Error::Abort(None));
                old_ready.set_promise_is_handled(cx);
                let new_ready = Promise::new(cx, &self.global());
                new_ready.resolve_native(cx, self);
                *self.ready_promise.borrow_mut() = new_ready.to_traced();
            }

            self.pending_finish_microtask.set(false);
            let old_finished = self.finished_promise.borrow().root(cx);
            old_finished.reject_error(cx, Error::Abort(None));
            old_finished.set_promise_is_handled(cx);
            let new_finished = Promise::new(cx, &self.global());
            *self.finished_promise.borrow_mut() = new_finished.to_traced();
            self.finished_promise_is_resolved.set(false);

            self.queue_playback_event(Atom::from("cancel"), None, self.timeline_current_time());
        }

        self.hold_time.set(None);
        self.start_time.set(None);
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-finish>
    fn Finish(&self, cx: &mut JSContext) -> ErrorResult {
        let effective_rate = self.effective_playback_rate();
        let end = self.associated_effect_end();
        if effective_rate == 0.0 || (effective_rate > 0.0 && end.is_infinite()) {
            return Err(Error::InvalidState(None));
        }

        self.apply_pending_playback_rate();
        let rate = self.playback_rate.get();
        let limit = if rate > 0.0 { end } else { 0.0 };

        self.hold_time.set(Some(limit));
        if let Some(timeline_time) = self.timeline_current_time() {
            self.start_time.set(Some(timeline_time - limit / rate));
        } else {
            self.start_time.set(None);
        }

        if self.pending_task.get() != PendingTask::None {
            self.pending_task.set(PendingTask::None);
            let ready = self.ready_promise.borrow().root(cx);
            ready.resolve_native(cx, self);
        }

        self.update_finished_state(cx, true, true);
        Ok(())
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-play>
    fn Play(&self, cx: &mut JSContext) -> ErrorResult {
        self.play_animation(cx, true)
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-pause>
    fn Pause(&self, cx: &mut JSContext) -> ErrorResult {
        let state = self.PlayState();
        if state == AnimationPlayState::Paused || self.pending_task.get() == PendingTask::Pause {
            return Ok(());
        }

        let has_pending_ready_promise = self.pending_task.get() != PendingTask::None;
        let effective_rate = self.effective_playback_rate();
        let end = self.associated_effect_end();
        let mut seek_time = None;

        if self.current_time().is_none() {
            if effective_rate >= 0.0 {
                seek_time = Some(0.0);
            } else if end.is_infinite() {
                return Err(Error::InvalidState(None));
            } else {
                seek_time = Some(end);
            }
        }

        if seek_time.is_some() {
            self.hold_time.set(seek_time);
        }

        if !has_pending_ready_promise {
            let new_ready = Promise::new(cx, &self.global());
            *self.ready_promise.borrow_mut() = new_ready.to_traced();
        }

        self.pending_task.set(PendingTask::Pause);
        self.register_with_document();
        self.update_finished_state(cx, false, false);
        Ok(())
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-updateplaybackrate>
    fn UpdatePlaybackRate(&self, cx: &mut JSContext, playback_rate: Finite<f64>) {
        self.pending_playback_rate.set(Some(*playback_rate));
        if self.pending_task.get() != PendingTask::None {
            return;
        }
        match self.PlayState() {
            AnimationPlayState::Idle | AnimationPlayState::Paused => {
                self.apply_pending_playback_rate();
            },
            AnimationPlayState::Finished => {
                let previous_time = self.current_time();
                self.apply_pending_playback_rate();
                if previous_time.is_some() {
                    self.silently_set_current_time(previous_time);
                }
                self.update_finished_state(cx, false, false);
            },
            AnimationPlayState::Running => {
                let _ = self.play_animation(cx, false);
            },
        }
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-reverse>
    fn Reverse(&self, cx: &mut JSContext) -> ErrorResult {
        if self.timeline_current_time().is_none() {
            return Err(Error::InvalidState(None));
        }
        let original_pending_rate = self.pending_playback_rate.get();
        self.pending_playback_rate
            .set(Some(-self.effective_playback_rate()));
        if let Err(err) = self.play_animation(cx, true) {
            self.pending_playback_rate.set(original_pending_rate);
            return Err(err);
        }
        Ok(())
    }

    /// <https://drafts.csswg.org/web-animations-1/#dom-animation-persist>
    fn Persist(&self) {
        self.replace_state.set(AnimationReplaceState::Persisted);
    }

    // https://drafts.csswg.org/web-animations-1/#dom-animation-onfinish
    event_handler!(finish, GetOnfinish, SetOnfinish);

    // https://drafts.csswg.org/web-animations-1/#dom-animation-oncancel
    event_handler!(cancel, GetOncancel, SetOncancel);

    // https://drafts.csswg.org/web-animations-1/#dom-animation-onremove
    event_handler!(remove, GetOnremove, SetOnremove);
}
