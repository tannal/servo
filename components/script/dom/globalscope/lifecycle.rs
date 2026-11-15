/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::any::Any;
use std::cell::Cell;

use js::jsapi::JSTracer;
use malloc_size_of::{MallocShallowSizeOf, MallocSizeOf, MallocSizeOfOps};
use script_bindings::cell::DomRefCell;
use script_bindings::weakref::{WeakRef, WeakReferenceable};

use crate::dom::bindings::trace::JSTraceable;

/// Implemented by resources bound to a [`crate::dom::globalscope::GlobalScope`]
/// that need explicit cleanup when the global scope is torn down (e.g., to
/// close off-heap threads/channels and release `Trusted` references held by
/// external callbacks).
pub(crate) trait LifecycleObserver {
    fn on_tear_down(&self);
}

trait ErasedLifecycleEntry: Any {
    fn is_alive(&self) -> bool;
    fn tear_down(&self);
    fn as_any(&self) -> &dyn Any;
}

#[cfg_attr(crown, crown::unrooted_must_root_lint::allow_unrooted_interior)]
struct WeakDomObserver<T: WeakReferenceable + LifecycleObserver + 'static>(WeakRef<T>);

impl<T: WeakReferenceable + LifecycleObserver + 'static> ErasedLifecycleEntry
    for WeakDomObserver<T>
{
    fn is_alive(&self) -> bool {
        self.0.is_alive()
    }

    fn tear_down(&self) {
        if let Some(observer) = self.0.root() {
            observer.on_tear_down();
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Per-`GlobalScope` registry of active resources that observe global teardown.
///
/// Entries are held weakly so normal garbage collection can still reclaim unused
/// DOM objects prior to teardown, while surviving entries are deterministically
/// torn down when the `GlobalScope` exits.
#[cfg_attr(crown, crown::unrooted_must_root_lint::allow_unrooted_interior)]
pub(crate) struct LifecycleRegistry {
    observers: DomRefCell<Vec<Box<dyn ErasedLifecycleEntry>>>,
    torn_down: Cell<bool>,
}

impl LifecycleRegistry {
    pub(crate) fn new() -> Self {
        Self {
            observers: DomRefCell::new(Vec::new()),
            torn_down: Cell::new(false),
        }
    }

    /// Register a weak-referenceable DOM observer with this `GlobalScope`.
    ///
    /// If the global scope has already been torn down (or is currently tearing down),
    /// `observer.on_tear_down()` is invoked synchronously instead of retaining it.
    pub(crate) fn register<T>(&self, observer: &T)
    where
        T: WeakReferenceable + LifecycleObserver + 'static,
    {
        if self.torn_down.get() {
            observer.on_tear_down();
            return;
        }

        let mut observers = self.observers.borrow_mut();
        if observers.len() == observers.capacity() {
            observers.retain(|entry| entry.is_alive());
        }
        observers.push(Box::new(WeakDomObserver(WeakRef::new(observer))));
    }

    /// Visit all currently live observers of concrete DOM type `T`.
    ///
    /// Snapshots weak references before dropping the internal borrow so callbacks
    /// may safely interact with `GlobalScope` or trigger GC.
    pub(crate) fn for_each<T, F>(&self, mut f: F)
    where
        T: WeakReferenceable + LifecycleObserver + 'static,
        F: FnMut(&T),
    {
        let matching: Vec<WeakRef<T>> = {
            let mut observers = self.observers.borrow_mut();
            let mut matching = Vec::new();
            observers.retain(|entry| {
                if !entry.is_alive() {
                    return false;
                }
                if let Some(weak_dom) = entry.as_any().downcast_ref::<WeakDomObserver<T>>() &&
                    let Some(observer) = weak_dom.0.root()
                {
                    matching.push(WeakRef::new(&*observer));
                }
                true
            });
            matching
        };

        for weak in matching {
            if let Some(observer) = weak.root() {
                f(&observer);
            }
        }
    }

    /// Tear down all registered observers. Idempotent across repeated calls.
    pub(crate) fn tear_down(&self) {
        if self.torn_down.get() {
            return;
        }
        self.torn_down.set(true);

        let observers = std::mem::take(&mut *self.observers.borrow_mut());
        for entry in observers {
            if entry.is_alive() {
                entry.tear_down();
            }
        }
    }
}

impl MallocSizeOf for LifecycleRegistry {
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        let observers = self.observers.borrow();
        observers.shallow_size_of(ops) +
            observers
                .iter()
                .map(|entry| std::mem::size_of_val(&**entry))
                .sum::<usize>()
    }
}

#[expect(unsafe_code)]
unsafe impl JSTraceable for LifecycleRegistry {
    unsafe fn trace(&self, _: *mut JSTracer) {
        self.observers.borrow_mut().retain(|entry| entry.is_alive());
    }
}
