/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::cell::Cell;

use js::jsapi::JSTracer;
use malloc_size_of::{MallocSizeOf, MallocSizeOfOps};
use script_bindings::cell::DomRefCell;
use script_bindings::weakref::{WeakRef, WeakReferenceable};

use crate::dom::bindings::root::DomRoot;
use crate::dom::bindings::trace::JSTraceable;

/// Lifecycle state of a `GlobalScope`'s resource teardown notifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, MallocSizeOf)]
pub(crate) enum LifecycleState {
    /// The global scope is live and accepting new observer registrations.
    Active,
    /// `notify_context_destroyed` is currently running.
    Destroying,
    /// The global scope has been torn down.
    Destroyed,
}

/// Implemented by DOM objects that hold external resources (network connections,
/// audio graphs, backend listeners, etc.) and need deterministic teardown when
/// their owning `GlobalScope` is destroyed.
///
/// `LifecycleNotifier` guarantees `on_context_destroyed` is invoked at most once
/// per registered observer.
pub(crate) trait LifecycleObserver: WeakReferenceable {
    fn on_context_destroyed(&self);
}

trait WeakLifecycleObserver {
    fn is_alive(&self) -> bool;
    fn upgrade(&self) -> Option<Box<dyn RootedLifecycleObserver>>;
}

trait RootedLifecycleObserver {
    fn on_context_destroyed(&self);
}

impl<T: LifecycleObserver> RootedLifecycleObserver for DomRoot<T> {
    fn on_context_destroyed(&self) {
        (**self).on_context_destroyed();
    }
}

impl<T: LifecycleObserver> WeakLifecycleObserver for WeakRef<T> {
    fn is_alive(&self) -> bool {
        WeakRef::is_alive(self)
    }

    fn upgrade(&self) -> Option<Box<dyn RootedLifecycleObserver>> {
        self.root()
            .map(|root| Box::new(root) as Box<dyn RootedLifecycleObserver>)
    }
}

/// Tracks weak references to `LifecycleObserver`s associated with a `GlobalScope`
/// and notifies them once when the context is torn down.
#[cfg_attr(crown, crown::unrooted_must_root_lint::allow_unrooted_interior)]
pub(crate) struct LifecycleNotifier {
    state: Cell<LifecycleState>,
    observers: DomRefCell<Vec<Box<dyn WeakLifecycleObserver>>>,
}

impl LifecycleNotifier {
    pub(crate) fn new() -> Self {
        Self {
            state: Cell::new(LifecycleState::Active),
            observers: DomRefCell::new(Vec::new()),
        }
    }

    /// Register a `LifecycleObserver` weakly. If the scope is already being
    /// destroyed or has been destroyed, `on_context_destroyed` is invoked
    /// immediately instead of registering.
    pub(crate) fn register<T: LifecycleObserver>(&self, observer: &T) {
        if self.state.get() != LifecycleState::Active {
            observer.on_context_destroyed();
            return;
        }
        self.observers
            .borrow_mut()
            .push(Box::new(WeakRef::new(observer)));
    }

    /// Notify all live observers that their owning context is being destroyed.
    ///
    /// This is a one-way latch: subsequent calls are no-ops, guaranteeing each
    /// observer receives `on_context_destroyed` at most once.
    pub(crate) fn notify_context_destroyed(&self) {
        if self.state.get() != LifecycleState::Active {
            return;
        }
        self.state.set(LifecycleState::Destroying);

        // Drain the weak list before invoking any callbacks so re-entrant
        // registrations cannot panic on `DomRefCell` borrow, and root all live
        // observers across the loop so a GC triggered mid-teardown cannot sweep
        // remaining observers before they are notified.
        let weak_observers = std::mem::take(&mut *self.observers.borrow_mut());
        let live_observers: Vec<Box<dyn RootedLifecycleObserver>> = weak_observers
            .into_iter()
            .filter_map(|observer| observer.upgrade())
            .collect();

        for observer in &live_observers {
            observer.on_context_destroyed();
        }

        self.state.set(LifecycleState::Destroyed);
    }
}

impl MallocSizeOf for LifecycleNotifier {
    #[expect(unsafe_code)]
    fn size_of(&self, ops: &mut MallocSizeOfOps) -> usize {
        let observers = self.observers.borrow();
        let vec_size = if observers.capacity() > 0 {
            unsafe { ops.malloc_size_of(observers.as_ptr()) }
        } else {
            0
        };
        vec_size + observers.len() * size_of::<std::rc::Weak<()>>()
    }
}

#[expect(unsafe_code)]
unsafe impl JSTraceable for LifecycleNotifier {
    unsafe fn trace(&self, _: *mut JSTracer) {
        // Prune weak references whose DOM objects were already collected.
        self.observers
            .borrow_mut()
            .retain(|observer| observer.is_alive());
    }
}
